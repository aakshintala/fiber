//! The 24-tool set the `wire_tools` tests send: 22 strict tools, one
//! loose schema and one enum with an object value.

use contract::provider::ToolDefinition;
use serde_json::json;

/// The tools every protocol's `wire_tools_is_what_the_request_sends` test
/// wires and sends: 22 strict tools in name order past the 20-strict cap,
/// plus `a_loose` and `z_enum`.
pub(crate) fn wire_tools_fixture() -> Vec<ToolDefinition> {
    let strict_schema = json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"],
        "additionalProperties": false
    });
    let mut tools: Vec<ToolDefinition> = (0..22)
        .map(|i| ToolDefinition {
            name: format!("tool_{i:02}"),
            description: "Weather for a city.".into(),
            input_schema: strict_schema.clone(),
            deferred: false,
        })
        .collect();
    tools.push(ToolDefinition {
        name: "a_loose".into(),
        description: "Loose.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["a"]
        }),
        deferred: false,
    });
    tools.push(ToolDefinition {
        name: "z_enum".into(),
        description: "Enum with an object value.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"pick": {"type": "string", "enum": [{"x": 1}]}},
            "required": ["pick"],
            "additionalProperties": false
        }),
        deferred: false,
    });
    tools
}
