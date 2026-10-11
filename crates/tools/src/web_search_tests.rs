use contract::ErrorCode;
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::{CancelToken, Recorder};
use serde_json::Map;

use super::HostedSearch;

fn search() -> HostedSearch {
    HostedSearch::new("web_search_20250305".into())
}

#[test]
fn the_definition_is_the_vendors_type_and_the_name_with_no_description() {
    let definition = search().definition();

    assert_eq!(definition.hosted.as_deref(), Some("web_search_20250305"));
    assert_eq!(definition.description, "");
}

#[test]
fn a_search_declares_network_reversible_with_an_empty_subject() {
    let effects = search().effects(&Map::new()).unwrap();

    assert_eq!(effects.declared.effects, vec![Effect::Network]);
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject.as_deref(), Some(""));
    assert_eq!(effects.prefix, None);
}

#[test]
fn the_guideline_tells_the_model_to_list_its_sources_as_markdown_links() {
    let guidelines = search().guidelines().unwrap();

    assert!(
        guidelines.contains("list of the sources you used, as markdown links"),
        "{guidelines}"
    );
}

#[test]
fn running_it_fails_because_the_provider_ran_the_search() {
    let output = search().run(&Map::new(), &CancelToken::new(), &Recorder::default());

    assert_eq!(output.error.unwrap().code, ErrorCode::ToolError);
}

use std::sync::{Arc, Mutex};

use contract::search::{Domains, SearchBackend, SearchResult};
use contract::shapes::Failure;
use contract::tool::{Bound, Cancel};
use serde_json::{Value, json};

use super::BackendSearch;

/// What a stub backend was called with and what it answers.
enum Reply {
    Results(Vec<SearchResult>),
    Failure(Failure),
    Cancelled,
}

struct Stub {
    calls: Mutex<Vec<(String, Domains)>>,
    reply: Reply,
}

impl Stub {
    fn results(results: Vec<SearchResult>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Reply::Results(results),
        }
    }

    fn failure(code: ErrorCode, message: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Reply::Failure(Failure {
                code,
                message: message.to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
        }
    }

    fn cancelled() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            reply: Reply::Cancelled,
        }
    }

    fn calls(&self) -> Vec<(String, Domains)> {
        self.calls.lock().unwrap().clone()
    }
}

impl SearchBackend for Stub {
    fn search(
        &self,
        query: &str,
        domains: &Domains,
        _cancel: &dyn Cancel,
    ) -> Result<Option<Vec<SearchResult>>, Failure> {
        self.calls
            .lock()
            .unwrap()
            .push((query.to_owned(), domains.clone()));
        match &self.reply {
            Reply::Results(results) => Ok(Some(results.clone())),
            Reply::Failure(failure) => Err(failure.clone()),
            Reply::Cancelled => Ok(None),
        }
    }
}

fn result(title: &str, url: &str, snippet: &str) -> SearchResult {
    SearchResult {
        title: title.to_owned(),
        url: url.to_owned(),
        snippet: snippet.to_owned(),
    }
}

fn backend(stub: Stub) -> (Arc<Stub>, BackendSearch) {
    let stub = Arc::new(stub);
    let tool = BackendSearch::new(Arc::clone(&stub) as Arc<dyn SearchBackend>);
    (stub, tool)
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn text(output: &contract::tool::Output) -> String {
    assert_eq!(output.error, None);
    assert_eq!(output.content.len(), 1);
    match &output.content[0] {
        contract::shapes::ContentPart::Text { text } => text.clone(),
        contract::shapes::ContentPart::Image { .. }
        | contract::shapes::ContentPart::Pdf(_)
        | contract::shapes::ContentPart::Unknown => {
            panic!("a text part, not {:?}", output.content[0])
        }
    }
}

#[test]
fn the_backend_definition_names_web_search_with_a_query_and_domain_filters() {
    let (_, tool) = backend(Stub::results(Vec::new()));
    let definition = tool.definition();
    assert_eq!(definition.name, "web_search");
    assert_eq!(definition.hosted, None);
    assert!(!definition.deferred);
    assert!(!definition.description.is_empty());
    let schema = definition.input_schema;
    assert_eq!(
        schema.get("required"),
        Some(&json!(["query"])),
        "the query is required"
    );
    assert_eq!(
        schema.pointer("/properties/query/minLength"),
        Some(&json!(1))
    );
    for filter in ["allowed_domains", "blocked_domains"] {
        assert_eq!(
            schema.pointer(&format!("/properties/{filter}/type")),
            Some(&json!("array")),
            "{filter} is a list"
        );
        assert_eq!(
            schema.pointer(&format!("/properties/{filter}/items/type")),
            Some(&json!("string")),
            "{filter} holds strings"
        );
    }
}

#[test]
fn a_backend_search_declares_what_a_hosted_search_declares() {
    let (_, tool) = backend(Stub::results(Vec::new()));
    assert_eq!(
        tool.effects(&Map::new()).unwrap(),
        search().effects(&Map::new()).unwrap()
    );
}

#[test]
fn the_backend_guideline_lists_sources_as_markdown_links() {
    let (_, tool) = backend(Stub::results(Vec::new()));
    let guidelines = tool.guidelines().unwrap();
    assert!(
        guidelines.contains("list of the sources you used, as markdown links"),
        "{guidelines}"
    );
}

#[test]
fn the_backend_tool_keeps_the_default_bound() {
    let (_, tool) = backend(Stub::results(Vec::new()));
    assert_eq!(tool.bound(), Bound::DEFAULT);
}

#[test]
fn the_query_reaches_the_backend_with_its_domain_filter() {
    let (stub, tool) = backend(Stub::results(Vec::new()));
    let cancel = CancelToken::new();
    let emit = Recorder::default();
    tool.run(&args(json!({"query": "rust"})), &cancel, &emit);
    tool.run(
        &args(json!({"query": "rust", "allowed_domains": ["rust-lang.org"]})),
        &cancel,
        &emit,
    );
    tool.run(
        &args(json!({"query": "rust", "blocked_domains": ["example.com"]})),
        &cancel,
        &emit,
    );
    assert_eq!(
        stub.calls(),
        [
            ("rust".to_owned(), Domains::Any),
            (
                "rust".to_owned(),
                Domains::Allowed(vec!["rust-lang.org".to_owned()])
            ),
            (
                "rust".to_owned(),
                Domains::Blocked(vec!["example.com".to_owned()])
            ),
        ]
    );
}

#[test]
fn both_domain_filters_fail_before_the_backend_is_called() {
    let (stub, tool) = backend(Stub::results(Vec::new()));
    let output = tool.run(
        &args(json!({"query": "rust", "allowed_domains": [], "blocked_domains": []})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(output.error.unwrap().code, ErrorCode::InvalidArguments);
    assert!(stub.calls().is_empty(), "the backend is not called");
}

#[test]
fn three_results_render_each_on_three_lines() {
    let (_, tool) = backend(Stub::results(vec![
        result(
            "Rust",
            "https://www.rust-lang.org/",
            "A language empowering everyone.",
        ),
        result(
            "The Book",
            "https://doc.rust-lang.org/book/",
            "The Rust Programming Language.",
        ),
        result("Docs", "https://docs.rs/", "API documentation."),
    ]));
    let output = tool.run(
        &args(json!({"query": "rust"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        text(&output),
        "1. Rust\n\
         https://www.rust-lang.org/\n\
         A language empowering everyone.\n\
         \n\
         2. The Book\n\
         https://doc.rust-lang.org/book/\n\
         The Rust Programming Language.\n\
         \n\
         3. Docs\n\
         https://docs.rs/\n\
         API documentation."
    );
}

#[test]
fn an_empty_list_renders_no_results() {
    let (_, tool) = backend(Stub::results(Vec::new()));
    let output = tool.run(
        &args(json!({"query": "rust"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(text(&output), "No results.");
}

#[test]
fn a_single_result_has_no_trailing_blank_line() {
    let (_, tool) = backend(Stub::results(vec![result(
        "Rust",
        "https://www.rust-lang.org/",
        "A language.",
    )]));
    let output = tool.run(
        &args(json!({"query": "rust"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        text(&output),
        "1. Rust\nhttps://www.rust-lang.org/\nA language."
    );
}

#[test]
fn a_backend_timeout_passes_through_with_its_code_and_message() {
    let (_, tool) = backend(Stub::failure(
        ErrorCode::Timeout,
        "the search passed its 50 ms timeout",
    ));
    let output = tool.run(
        &args(json!({"query": "rust"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(error.message, "the search passed its 50 ms timeout");
}

#[test]
fn a_cancelled_search_returns_no_content_and_no_error() {
    let (_, tool) = backend(Stub::cancelled());
    let output = tool.run(
        &args(json!({"query": "rust"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(output, contract::tool::Output::default());
}
