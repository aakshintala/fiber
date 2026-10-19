use contract::ErrorCode;
use contract::provider::{NO_MATCH, TOOL_SEARCH, ToolDefinition, loaded_text};
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

fn message(output: &contract::tool::Output) -> String {
    output
        .error
        .as_ref()
        .map(|error| error.message.clone())
        .unwrap_or_default()
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

#[test]
fn a_negative_limit_names_the_minimum() {
    let arguments = fakes::tool_args(json!({"query": "plumbus", "limit": -1}));
    let output = run(&search(), &arguments);

    assert_eq!(
        message(&output),
        "The arguments do not fit the schema: limit must be at least 1."
    );
    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}

#[test]
fn a_fractional_limit_names_the_integer() {
    let arguments = fakes::tool_args(json!({"query": "plumbus", "limit": 1.5}));
    let output = run(&search(), &arguments);

    assert_eq!(
        message(&output),
        "The arguments do not fit the schema: limit must be an integer of at least 1."
    );
    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}

#[test]
fn a_fractional_limit_past_u64_is_an_invalid_argument() {
    // `18446744073709551616.5` reads back as 2^64, exactly like the integer
    // past `u64`: the fraction is already lost, so every oversized number
    // fails as a non-integer.
    let past_u64: Value = serde_json::from_str("18446744073709551616.5").unwrap();
    let output = run(&matching_search(9), &widget_query(past_u64));

    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}

/// Every tool matches `widget`, with identical scores, so the load order
/// is the name order.
fn matching_catalogue(count: usize) -> Vec<ToolDefinition> {
    (0..count)
        .map(|index| leaf(&format!("cap_{index:02}"), "widget provider"))
        .collect()
}

fn matching_search(count: usize) -> ToolSearch {
    ToolSearch::new(matching_catalogue(count), vec![])
}

fn widget_query(limit: Value) -> Map<String, Value> {
    let mut arguments = query("widget");
    arguments.insert("limit".to_owned(), limit);
    arguments
}

#[test]
fn the_default_cap_at_seven_matching_tools() {
    let output = run(&matching_search(7), &query("widget"));

    assert_eq!(output.loaded.map(|names| names.len()), Some(7));
}

#[test]
fn the_default_cap_at_eight_matching_tools() {
    let output = run(&matching_search(8), &query("widget"));

    assert_eq!(output.loaded.map(|names| names.len()), Some(8));
}

#[test]
fn the_default_cap_at_nine_matching_tools() {
    let output = run(&matching_search(9), &query("widget"));

    assert_eq!(
        loaded(&output),
        Some(vec![
            "cap_00".to_owned(),
            "cap_01".to_owned(),
            "cap_02".to_owned(),
            "cap_03".to_owned(),
            "cap_04".to_owned(),
            "cap_05".to_owned(),
            "cap_06".to_owned(),
            "cap_07".to_owned(),
        ])
    );
}

#[test]
fn a_limit_of_one_loads_one() {
    let output = run(&matching_search(9), &widget_query(json!(1)));

    assert_eq!(loaded(&output), Some(vec!["cap_00".to_owned()]));
}

#[test]
fn a_limit_of_two_loads_two() {
    let output = run(&matching_search(9), &widget_query(json!(2)));

    assert_eq!(
        loaded(&output),
        Some(vec!["cap_00".to_owned(), "cap_01".to_owned()])
    );
}

#[test]
fn a_limit_past_the_matches_loads_every_match() {
    let output = run(&matching_search(9), &widget_query(json!(20)));

    assert_eq!(loaded(&output).map(|names| names.len()), Some(9));
}

#[test]
fn a_limit_past_u64_is_an_invalid_argument() {
    // The integer past `u64` reads back as a float, exactly like a
    // fractional value there: with the fraction already lost it cannot
    // prove it was integral, so it fails as a non-integer.
    let past_u64: Value = serde_json::from_str("18446744073709551616").unwrap();
    let output = run(&matching_search(9), &widget_query(past_u64));

    assert_eq!(
        output.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(output.loaded, None);
}

#[test]
fn effects_contact_no_host() {
    let effects = search()
        .effects(&query("plumbus"))
        .expect("valid arguments have effects");

    assert_eq!(effects.hosts, Some(vec![]));
}

#[test]
fn the_definition_names_the_tool_and_never_defers() {
    let definition = search().definition();

    assert_eq!(definition.name, TOOL_SEARCH);
    assert_eq!(definition.name, "tool_search");
    assert!(!definition.deferred);
}

#[test]
fn the_schema_matches_the_interfaces_literal() {
    let definition = search().definition();

    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Words describing the tool you need."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The most tools to load; 8 when absent."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    );
}

#[test]
fn the_description_names_the_sources() {
    let definition = search().definition();

    assert_eq!(
        definition.description,
        "Searches the deferred tools by name, description and parameter names, and loads the best matches so you can call them. Deferred tools come from: fx, gh."
    );
}

#[test]
fn the_description_without_sources_is_the_base_sentence() {
    let definition = ToolSearch::new(vec![], vec![]).definition();

    assert_eq!(
        definition.description,
        "Searches the deferred tools by name, description and parameter names, and loads the best matches so you can call them."
    );
}

#[test]
fn the_guideline_is_the_tool_search_section() {
    let guideline = search().guidelines();

    assert_eq!(guideline, crate::guidelines::of("tool_search"));
    assert!(
        guideline
            .as_deref()
            .is_some_and(|text| text.contains("When a tool you need is missing")),
        "{guideline:?}"
    );
}
