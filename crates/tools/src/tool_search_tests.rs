use contract::ErrorCode;
use contract::provider::{NO_MATCH, ToolDefinition, loaded_text};
use contract::shapes::ContentPart;
use contract::tool::Tool;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::ToolSearch;

fn run(search: &ToolSearch, arguments: &Map<String, Value>) -> contract::tool::Output {
    search.run(arguments, &CancelToken::new(), &Recorder::default())
}

fn query(value: &str) -> Map<String, Value> {
    fakes::tool_args(json!({"query": value}))
}

fn catalogue_tool(name: &str, description: &str, properties: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({"type": "object", "properties": properties}),
        deferred: true,
        hosted: None,
    }
}

fn leaf(name: &str, description: &str) -> ToolDefinition {
    catalogue_tool(name, description, json!({}))
}

/// The fixed catalogue: disjoint vocabularies per case, so each targeted
/// query matches exactly its tools.
fn catalogue() -> Vec<ToolDefinition> {
    vec![
        leaf("zeta_plumbus", "plumbus plumbus plumbus helper"),
        leaf("beta_plumbus", "plumbus helper"),
        leaf("gamma_other", "plumbus"),
        leaf("xylophone_player", "Makes music."),
        leaf("quiet_tool", "Calibrates the sprocket."),
        catalogue_tool(
            "assembler",
            "Builds widgets.",
            json!({"gizmo": {"type": "string"}}),
        ),
        catalogue_tool(
            "nester",
            "Holds things.",
            json!({"outer": {"type": "object", "properties": {"quuxhidden": {"type": "string"}}}}),
        ),
        leaf("mcp__fx__echo", "Returns its input."),
        leaf("Shouter", "LOUD NOISES"),
        leaf("B_tied", "tied pair words"),
        leaf("a_tied", "tied pair words"),
    ]
}

fn search() -> ToolSearch {
    ToolSearch::new(catalogue(), vec!["fx".to_owned(), "gh".to_owned()])
}

fn loaded(output: &contract::tool::Output) -> Option<Vec<String>> {
    output.loaded.clone()
}

#[test]
fn rank_order_on_the_fixed_catalogue() {
    let output = run(&search(), &query("plumbus"));

    assert_eq!(
        loaded(&output),
        Some(vec![
            "zeta_plumbus".to_owned(),
            "beta_plumbus".to_owned(),
            "gamma_other".to_owned(),
        ])
    );
}

#[test]
fn a_name_only_hit_is_found() {
    let output = run(&search(), &query("xylophone"));

    assert_eq!(loaded(&output), Some(vec!["xylophone_player".to_owned()]));
}

#[test]
fn a_description_only_hit_is_found() {
    let output = run(&search(), &query("sprocket"));

    assert_eq!(loaded(&output), Some(vec!["quiet_tool".to_owned()]));
}

#[test]
fn a_parameter_name_only_hit_is_found() {
    let output = run(&search(), &query("gizmo"));

    assert_eq!(loaded(&output), Some(vec!["assembler".to_owned()]));
}

#[test]
fn a_nested_property_name_is_not_indexed() {
    let output = run(&search(), &query("quuxhidden"));

    assert_eq!(loaded(&output), Some(vec![]));
}

#[test]
fn a_top_level_property_name_is_indexed() {
    let output = run(&search(), &query("outer"));

    assert_eq!(loaded(&output), Some(vec!["nester".to_owned()]));
}

#[test]
fn an_underscored_name_is_found_by_one_token() {
    let output = run(&search(), &query("echo"));

    assert_eq!(loaded(&output), Some(vec!["mcp__fx__echo".to_owned()]));
}

#[test]
fn matching_ignores_case() {
    let upper_query = run(&search(), &query("ECHO"));
    let upper_document = run(&search(), &query("loud"));

    assert_eq!(loaded(&upper_query), Some(vec!["mcp__fx__echo".to_owned()]));
    assert_eq!(loaded(&upper_document), Some(vec!["Shouter".to_owned()]));
}

#[test]
fn equal_scores_order_by_name_bytes() {
    let output = run(&search(), &query("tied"));

    assert_eq!(
        loaded(&output),
        Some(vec!["B_tied".to_owned(), "a_tied".to_owned()])
    );
}

#[test]
fn a_repeated_query_gives_the_same_list() {
    let search = search();
    let first = run(&search, &query("plumbus"));
    let second = run(&search, &query("plumbus"));

    assert_eq!(loaded(&first), loaded(&second));
    assert_eq!(
        loaded(&first),
        Some(vec![
            "zeta_plumbus".to_owned(),
            "beta_plumbus".to_owned(),
            "gamma_other".to_owned(),
        ])
    );
}

#[test]
fn zero_matches_say_so() {
    let output = run(&search(), &query("?!"));

    assert_eq!(loaded(&output), Some(vec![]));
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: NO_MATCH.to_owned(),
        }]
    );
    assert_eq!(NO_MATCH, "No deferred tool matched the query.");
}

#[test]
fn a_hit_reports_the_loaded_names() {
    let output = run(&search(), &query("plumbus"));
    let names = vec![
        "zeta_plumbus".to_owned(),
        "beta_plumbus".to_owned(),
        "gamma_other".to_owned(),
    ];

    assert_eq!(loaded(&output), Some(names.clone()));
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: loaded_text(&names),
        }]
    );
}

#[test]
fn an_empty_query_is_an_invalid_argument() {
    let output = run(&search(), &query(""));

    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}

#[test]
fn a_zero_limit_is_an_invalid_argument() {
    let arguments = fakes::tool_args(json!({"query": "plumbus", "limit": 0}));
    let output = run(&search(), &arguments);

    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}
