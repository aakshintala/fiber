//! One MCP elicitation Fiber can show (`docs/mcp.md`, "Elicitation,
//! sampling and roots"): a form of typed fields read in the server's
//! order, each asked as the interaction the doc's table names, and
//! answered field by field. Only the loop thread writes the interaction
//! lines; this module emits no durable output.

use std::collections::BTreeSet;
use std::time::Instant;

use contract::ActionId;
use contract::events::{Answer, Interaction};
use contract::shapes::Choice;
use contract::tool::{Answered, Ask, Asking, Check};
use serde::Deserialize;
use serde_json::Value;

/// One `elicitation/create` Fiber can show (`docs/mcp.md`, "Elicitation,
/// sampling and roots").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Form {
    /// What the server asks for.
    pub(crate) message: String,
    /// The fields in the server's order, never empty.
    pub(crate) fields: Vec<Field>,
}

/// One field of a form.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Field {
    /// The property name, sent back as the content key.
    pub(crate) name: String,
    /// The property's `title`, or its name when it has none.
    pub(crate) title: Option<String>,
    /// The property's `description`, when it has one.
    pub(crate) description: Option<String>,
    /// Whether the field is in `required`.
    pub(crate) required: bool,
    /// The field's kind.
    pub(crate) kind: FieldKind,
}

/// One field's kind, with the limits Fiber checks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FieldKind {
    /// A boolean: a `confirm`.
    Boolean,
    /// A string with `enum`: a `select`. Values are distinct, and
    /// `names.len() == values.len()` when names are present.
    Enum {
        /// The allowed values.
        values: Vec<String>,
        /// The matching `enumNames`, when the server sent any.
        names: Option<Vec<String>>,
    },
    /// A string without `enum`: a text input, checked by length.
    Text {
        /// The `minLength`, when the server sent one.
        min: Option<usize>,
        /// The `maxLength`, when the server sent one.
        max: Option<usize>,
    },
    /// A number: a text input, compared as `f64` and sent as an `f64`
    /// JSON number, so the comparison is exact for what is sent.
    Number {
        /// The inclusive `minimum`, when the server sent one.
        min: Option<f64>,
        /// The inclusive `maximum`, when the server sent one.
        max: Option<f64>,
    },
    /// An integer: a text input, compared exactly without `f64`. Each
    /// bound is one effective inclusive integer, clamped to
    /// `[i64::MIN - 1, i64::MAX + 1]` when read.
    Integer {
        /// The effective inclusive `minimum`, when the server sent one.
        min: Option<i128>,
        /// The effective inclusive `maximum`, when the server sent one.
        max: Option<i128>,
    },
}

/// Who a call asks through, and the server's configured name for its prompts.
#[derive(Clone, Copy)]
pub(crate) struct Asker<'a> {
    /// The call's asker.
    pub(crate) ask: &'a dyn Ask,
    /// The server's configured name, naming it in each prompt.
    pub(crate) server: &'a str,
}

/// The raw request line's `params`. Every shape above the properties is a
/// `Value`, so a wrong shape becomes this module's reason naming the key;
/// `properties` alone deserialises through the ordered visitor below, so
/// the fields keep the server's document order (`serde_json::Map` is
/// sorted, so `Value` would lose it). A wrong shape at or above
/// `properties` fails the whole parse, and `read` names it by walking the
/// line again as a `Value`.
#[derive(Deserialize)]
struct Line {
    /// The request's `params`.
    params: Option<Params>,
}

/// An elicitation's `params`.
#[derive(Deserialize)]
struct Params {
    /// What the server asks for.
    message: Option<Value>,
    /// The form schema.
    #[serde(rename = "requestedSchema")]
    requested_schema: Option<Schema>,
}

/// An elicitation's `requestedSchema`.
#[derive(Deserialize)]
struct Schema {
    /// The schema's `type`, always `"object"` for a form.
    #[serde(rename = "type")]
    schema_type: Option<Value>,
    /// The properties in the server's document order.
    properties: Option<OrderedProperties>,
    /// The required property names.
    required: Option<Value>,
}

/// The schema's properties in the server's document order. `serde_json`
/// visits map entries in document order, so collecting into a vector
/// keeps it; a map would sort it.
struct OrderedProperties(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedProperties {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Order;

        impl<'de> serde::de::Visitor<'de> for Order {
            type Value = OrderedProperties;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object of properties")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut properties = Vec::with_capacity(map.size_hint().unwrap_or(0));
                // The repeat check is a set, so a large schema is not quadratic.
                let mut seen = BTreeSet::new();
                while let Some((name, schema)) = map.next_entry::<String, Value>()? {
                    if !seen.insert(name.clone()) {
                        // Read back verbatim by `read`: the marker is this
                        // module's own prefix, never a serde error's.
                        return Err(serde::de::Error::custom(format!(
                            "repeated property `{name}`."
                        )));
                    }
                    properties.push((name, schema));
                }
                Ok(OrderedProperties(properties))
            }
        }

        deserializer.deserialize_map(Order)
    }
}

/// Reads the raw request line; `Err` is the reason, without the "Fiber
/// cannot show" prefix `pipes` adds.
pub(crate) fn read(line: &str) -> Result<Form, String> {
    match serde_json::from_str::<Line>(line) {
        Ok(line) => build(line),
        Err(error) => {
            let message = error.to_string();
            // The parser appends its position to the visitor's message,
            // so only the property name is kept for the reason.
            if let Some(rest) = message.strip_prefix("repeated property `")
                && let Some((name, _)) = rest.split_once('`')
            {
                return Err(format!("property `{name}` is listed twice."));
            }
            Err(fallback_reason(line))
        }
    }
}

/// Builds the form from a parsed line, naming the key or property every
/// rejection meets.
fn build(line: Line) -> Result<Form, String> {
    let params = match line.params {
        Some(params) => params,
        None => return Err("`params` is missing.".to_owned()),
    };
    let message = match params.message {
        Some(Value::String(message)) => message,
        Some(_) | None => return Err("`message` must be a string.".to_owned()),
    };
    let schema = match params.requested_schema {
        Some(schema) => schema,
        None => return Err("`requestedSchema` must be an object.".to_owned()),
    };
    match &schema.schema_type {
        Some(Value::String(schema_type)) if schema_type == "object" => {}
        Some(_) | None => return Err("`requestedSchema.type` must be `\"object\"`.".to_owned()),
    }
    let properties = match schema.properties {
        Some(OrderedProperties(properties)) if !properties.is_empty() => properties,
        Some(_) | None => {
            return Err("`requestedSchema.properties` must be a non-empty object.".to_owned());
        }
    };
    let required = match schema.required {
        None => BTreeSet::new(),
        Some(Value::Array(required)) => {
            let mut names = BTreeSet::new();
            for entry in required {
                match entry {
                    Value::String(name) if properties.iter().any(|(listed, _)| listed == &name) => {
                        names.insert(name);
                    }
                    Value::String(name) => {
                        return Err(format!("`required` names unknown property `{name}`."));
                    }
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::Array(_)
                    | Value::Object(_) => {
                        return Err("`required` must be an array of strings.".to_owned());
                    }
                }
            }
            names
        }
        Some(_) => return Err("`required` must be an array of strings.".to_owned()),
    };
    let mut fields = Vec::with_capacity(properties.len());
    for (name, schema) in &properties {
        let mut field = read_field(name, schema)?;
        field.required = required.contains(field.name.as_str());
        fields.push(field);
    }
    Ok(Form { message, fields })
}

/// Names the key the raw parse failed on, by walking the line as a
/// `Value`: the failure is at or above the properties (a wrong shape),
/// never in a single field, which `build` reports.
fn fallback_reason(line: &str) -> String {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return "`params` is missing.".to_owned(),
    };
    let params = match value.get("params") {
        Some(Value::Object(_)) => match value.get("params") {
            Some(params) => params,
            None => return "`params` must be an object.".to_owned(),
        },
        Some(_) | None => return "`params` must be an object.".to_owned(),
    };
    match params.get("message") {
        Some(Value::String(_)) => {}
        Some(_) | None => return "`message` must be a string.".to_owned(),
    }
    let schema = match params.get("requestedSchema") {
        Some(Value::Object(_)) => match params.get("requestedSchema") {
            Some(schema) => schema,
            None => return "`requestedSchema` must be an object.".to_owned(),
        },
        Some(_) | None => return "`requestedSchema` must be an object.".to_owned(),
    };
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {}
        Some(_) | None => return "`requestedSchema.type` must be `\"object\"`.".to_owned(),
    }
    match schema.get("properties") {
        Some(Value::Object(properties)) if !properties.is_empty() => {}
        Some(_) | None => {
            return "`requestedSchema.properties` must be a non-empty object.".to_owned();
        }
    }
    "`requestedSchema.properties` must be a non-empty object.".to_owned()
}

/// Reads one property into a field; `Err` names the property.
fn read_field(name: &str, schema: &Value) -> Result<Field, String> {
    let object = match schema.as_object() {
        Some(object) => object,
        None => return Err(format!("property `{name}` must be an object.")),
    };
    let field_type = match object.get("type").and_then(Value::as_str) {
        Some(field_type) => field_type,
        None => return Err(format!("property `{name}` has no `type`.")),
    };
    let title = match object.get("title") {
        None => None,
        Some(Value::String(title)) => Some(title.clone()),
        Some(_) => return Err(format!("property `{name}`'s `title` is not a string.")),
    };
    let description = match object.get("description") {
        None => None,
        Some(Value::String(description)) => Some(description.clone()),
        Some(_) => {
            return Err(format!(
                "property `{name}`'s `description` is not a string."
            ));
        }
    };
    let kind = match field_type {
        "boolean" => FieldKind::Boolean,
        "string" => read_string(name, object)?,
        "number" => read_number(name, object)?,
        "integer" => read_integer(name, object)?,
        _ => return Err(format!("property `{name}` has type `{field_type}`.")),
    };
    Ok(Field {
        name: name.to_owned(),
        title,
        description,
        required: false,
        kind,
    })
}

/// Reads a string property: with `enum` a select, without it a text
/// input. Any other key, such as `format` or `pattern`, is ignored: the
/// server checks it.
fn read_string(name: &str, object: &serde_json::Map<String, Value>) -> Result<FieldKind, String> {
    match object.get("enum") {
        None => {
            let min = read_length(name, object, "minLength")?;
            let max = read_length(name, object, "maxLength")?;
            Ok(FieldKind::Text { min, max })
        }
        Some(Value::Array(values)) => {
            if values.is_empty() {
                return Err(format!("property `{name}`'s `enum` must not be empty."));
            }
            let mut distinct = Vec::with_capacity(values.len());
            let mut seen = BTreeSet::new();
            for value in values {
                match value.as_str() {
                    Some(label) if seen.insert(label.to_owned()) => {
                        distinct.push(label.to_owned());
                    }
                    Some(label) => {
                        return Err(format!("property `{name}`'s `enum` repeats `{label}`."));
                    }
                    None => {
                        return Err(format!(
                            "property `{name}`'s `enum` must hold only strings."
                        ));
                    }
                }
            }
            let names = match object.get("enumNames") {
                None => None,
                Some(Value::Array(names))
                    if names.len() == distinct.len() && names.iter().all(Value::is_string) =>
                {
                    Some(
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                    )
                }
                Some(_) => {
                    return Err(format!(
                        "property `{name}`'s `enumNames` must be strings matching its `enum`."
                    ));
                }
            };
            Ok(FieldKind::Enum {
                values: distinct,
                names,
            })
        }
        Some(_) => Err(format!("property `{name}`'s `enum` must be an array.")),
    }
}

/// Reads a `minLength` or `maxLength`: absent, or a non-negative integer.
fn read_length(
    name: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<usize>, String> {
    match object.get(key) {
        None => Ok(None),
        // `as_u64` refuses fractions, floats and negatives alike.
        Some(value) => match value.as_u64() {
            Some(bound) => match usize::try_from(bound) {
                Ok(bound) => Ok(Some(bound)),
                Err(_) => Err(format!("property `{name}`'s `{key}` is too large.")),
            },
            None => Err(format!(
                "property `{name}`'s `{key}` must be a non-negative integer."
            )),
        },
    }
}

/// Reads a number property's `minimum` and `maximum`: absent, or a
/// number, compared as `f64`. An `enum` belongs only on a string.
fn read_number(name: &str, object: &serde_json::Map<String, Value>) -> Result<FieldKind, String> {
    if object.contains_key("enum") {
        return Err(format!(
            "only a string property may have `enum`, not `{name}`."
        ));
    }
    let min = read_float(name, object, "minimum")?;
    let max = read_float(name, object, "maximum")?;
    Ok(FieldKind::Number { min, max })
}

/// Reads a `minimum` or `maximum` of a number field: absent, or a number.
fn read_float(
    name: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<f64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Number(bound)) => match bound.as_f64() {
            Some(bound) => Ok(Some(bound)),
            None => Err(format!("property `{name}`'s `{key}` must be a number.")),
        },
        Some(_) => Err(format!("property `{name}`'s `{key}` must be a number.")),
    }
}

/// Reads an integer property's `minimum` and `maximum` into effective
/// inclusive integers. An exact `i64` or `u64` bound is taken as it is;
/// any `f64` bound (a fraction, an integral float, or past the integer
/// range) becomes `ceil` for a minimum and `floor` for a maximum, clamped
/// before its one checked cast, so no conversion can overflow. An `enum`
/// belongs only on a string.
fn read_integer(name: &str, object: &serde_json::Map<String, Value>) -> Result<FieldKind, String> {
    if object.contains_key("enum") {
        return Err(format!(
            "only a string property may have `enum`, not `{name}`."
        ));
    }
    let min = read_effective(name, object, "minimum", true)?;
    let max = read_effective(name, object, "maximum", false)?;
    Ok(FieldKind::Integer { min, max })
}

/// Reads one integer bound into its effective inclusive `i128`, clamped
/// to `[i64::MIN - 1, i64::MAX + 1]`.
fn read_effective(
    name: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
    is_minimum: bool,
) -> Result<Option<i128>, String> {
    let bound = match object.get(key) {
        None => return Ok(None),
        Some(bound) => bound,
    };
    let number = match bound.as_number() {
        Some(number) => number,
        None => return Err(format!("property `{name}`'s `{key}` must be a number.")),
    };
    if let Some(exact) = number.as_i64() {
        return Ok(Some(i128::from(exact)));
    }
    if let Some(exact) = number.as_u64() {
        return Ok(Some(i128::from(exact)));
    }
    match number.as_f64() {
        Some(float) => {
            let edged = if is_minimum {
                float.ceil()
            } else {
                float.floor()
            };
            // Both bounds are exactly representable powers of two, so
            // the range check below leaves every in-range integral float
            // to the exact cast after it.
            const BELOW: f64 = -9223372036854775808.0;
            const ABOVE: f64 = 9223372036854775808.0;
            if edged < BELOW {
                Ok(Some(i128::from(i64::MIN) - 1))
            } else if edged > ABOVE {
                Ok(Some(i128::from(i64::MAX) + 1))
            } else {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the range check just above leaves only in-range integral floats"
                )]
                let exact = edged as i64;
                Ok(Some(i128::from(exact)))
            }
        }
        None => Err(format!("property `{name}`'s `{key}` must be a number.")),
    }
}

/// The interaction for one field (`docs/mcp.md`, "Elicitation, sampling
/// and roots"): the kind the doc's table names, with a prompt naming the
/// server, as the spec asks clients to show which server asks.
pub(crate) fn interaction(server: &str, form: &Form, field: &Field) -> Interaction {
    let subject = field.title.as_deref().unwrap_or(&field.name);
    let mut prompt = format!("The MCP server `{server}` asks: {}", form.message);
    prompt.push_str("\n\n");
    prompt.push_str(subject);
    if let Some(description) = &field.description {
        prompt.push_str(": ");
        prompt.push_str(description);
    }
    match &field.kind {
        FieldKind::Boolean => Interaction::Confirm { prompt },
        FieldKind::Enum { values, names } => {
            let options = values
                .iter()
                .enumerate()
                .map(|(index, label)| Choice {
                    label: label.clone(),
                    description: names.as_ref().and_then(|names| names.get(index)).cloned(),
                })
                .collect();
            Interaction::Select { prompt, options }
        }
        FieldKind::Text { .. } | FieldKind::Number { .. } | FieldKind::Integer { .. } => {
            Interaction::TextInput { prompt }
        }
    }
}

/// The check on a typed answer (`docs/invocation.md`, "Replying"): a
/// text input from a number, integer or string field. `confirm` and
/// `select` get none: `Interaction::fit` already checks them. A failing
/// answer is rejected `invalid_arguments` and the field stays pending.
pub(crate) fn check(field: &Field) -> Option<Check> {
    match &field.kind {
        FieldKind::Boolean | FieldKind::Enum { .. } => None,
        FieldKind::Text { min, max } => {
            let (required, min, max) = (field.required, *min, *max);
            Some(Box::new(move |answer: &Answer| match answer {
                Answer::Text { text } => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() && !required {
                        return true;
                    }
                    let len = trimmed.chars().count();
                    min.is_none_or(|min| len >= min) && max.is_none_or(|max| len <= max)
                }
                Answer::Declined { .. }
                | Answer::Confirmed { .. }
                | Answer::Labels { .. }
                | Answer::Form { .. } => false,
            }))
        }
        FieldKind::Number { min, max } => {
            let (required, min, max) = (field.required, *min, *max);
            Some(Box::new(move |answer: &Answer| match answer {
                Answer::Text { text } => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() && !required {
                        return true;
                    }
                    match trimmed.parse::<f64>() {
                        Ok(value) if value.is_finite() => {
                            min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
                        }
                        Ok(_) | Err(_) => false,
                    }
                }
                Answer::Declined { .. }
                | Answer::Confirmed { .. }
                | Answer::Labels { .. }
                | Answer::Form { .. } => false,
            }))
        }
        FieldKind::Integer { min, max } => {
            let (required, min, max) = (field.required, *min, *max);
            Some(Box::new(move |answer: &Answer| match answer {
                Answer::Text { text } => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() && !required {
                        return true;
                    }
                    match trimmed.parse::<i64>() {
                        Ok(value) => {
                            let value = i128::from(value);
                            min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
                        }
                        Err(_) => false,
                    }
                }
                Answer::Declined { .. }
                | Answer::Confirmed { .. }
                | Answer::Labels { .. }
                | Answer::Form { .. } => false,
            }))
        }
    }
}

/// What one answered field becomes: a value for `content`, left out, or
/// the whole elicitation declined.
enum Valued {
    /// A value for `content`.
    Value(Value),
    /// An optional field left out of `content`.
    Omitted,
    /// The whole elicitation is declined.
    Decline,
}

/// Reads one answer into its content value. A value of the wrong kind
/// cannot reach here (the loop fits it to the interaction first); when
/// one does, the whole elicitation is declined rather than partly
/// accepted.
fn value(field: &Field, answer: &Answer) -> Valued {
    match (answer, &field.kind) {
        (Answer::Declined { .. }, _) if field.required => Valued::Decline,
        (Answer::Declined { .. }, _) => Valued::Omitted,
        (Answer::Confirmed { confirmed }, FieldKind::Boolean) => {
            Valued::Value(Value::Bool(*confirmed))
        }
        (Answer::Labels { labels }, FieldKind::Enum { values, .. }) => match labels.as_slice() {
            [label] if values.contains(label) => Valued::Value(Value::String(label.clone())),
            _ => Valued::Decline,
        },
        (Answer::Text { text }, FieldKind::Text { .. }) => {
            let trimmed = text.trim();
            if trimmed.is_empty() && !field.required {
                Valued::Omitted
            } else {
                Valued::Value(Value::String(trimmed.to_owned()))
            }
        }
        (Answer::Text { text }, FieldKind::Number { .. }) => match text.trim().parse::<f64>() {
            Ok(number) if number.is_finite() => Valued::Value(
                serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number),
            ),
            Ok(_) | Err(_) => Valued::Decline,
        },
        (Answer::Text { text }, FieldKind::Integer { .. }) => match text.trim().parse::<i64>() {
            Ok(number) => Valued::Value(Value::Number(number.into())),
            Err(_) => Valued::Decline,
        },
        (Answer::Confirmed { .. }, _)
        | (Answer::Labels { .. }, _)
        | (Answer::Text { .. }, _)
        | (Answer::Form { .. }, _) => Valued::Decline,
    }
}

/// Asks each field in order; returns the `result` object to send: every
/// field answered is `{"action":"accept","content":{...}}`; a required
/// field answered `declined`, or `NoAnswer` on any field, is
/// `{"action":"decline"}` at once, and later fields are not asked. A
/// declined optional field is left out and the rest is still accepted.
/// Fiber never sends `cancel`.
pub(crate) fn respond(
    asker: Asker<'_>,
    form: &Form,
    action_ids: Vec<ActionId>,
    until: Instant,
) -> Value {
    let mut content = serde_json::Map::new();
    for field in &form.fields {
        let asking = Asking {
            interaction: interaction(asker.server, form, field),
            action_ids: action_ids.clone(),
            until: Some(until),
            check: check(field),
            suspends: false,
        };
        match asker.ask.ask(asking) {
            Answered::Reply(answer) => match value(field, &answer) {
                Valued::Value(value) => {
                    content.insert(field.name.clone(), value);
                }
                Valued::Omitted => {}
                Valued::Decline => return serde_json::json!({"action": "decline"}),
            },
            Answered::NoAnswer => return serde_json::json!({"action": "decline"}),
        }
    }
    serde_json::json!({"action": "accept", "content": Value::Object(content)})
}

#[cfg(test)]
#[path = "elicit_tests.rs"]
pub(crate) mod tests;
