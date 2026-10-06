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

    assert_eq!(definition.name, "web_search");
    assert_eq!(definition.hosted.as_deref(), Some("web_search_20250305"));
    assert_eq!(definition.description, "");
    assert!(!definition.deferred);
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
