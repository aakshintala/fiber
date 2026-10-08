//! Direct calls to a package's Lua provider (`docs/testing.md`, "Testing an extension").

use std::sync::Arc;

use config::Secret;
use contract::GenerationId;
use contract::clock::Clock;
use serde_json::{Value, json};

use super::clock::CaseClock;
use super::format::Case;
use super::run::report;

/// Runs a provider call without creating a session.
pub(crate) fn run_case(case: Case, name: String, process_clock: Arc<dyn Clock>) -> i32 {
    let Some(call) = case.call else {
        return report(&name, &["call: required for a call case".to_owned()]);
    };
    if let Err(error) = function(&call.function) {
        return report(&name, &[error]);
    }
    let arg = match cost_args(&call.arg) {
        Ok(arg) => arg,
        Err(error) => return report(&name, &[error]),
    };
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return report(&name, &[error.to_string()]),
    };
    let installed = match extensions::list(&home, process_clock.as_ref()) {
        Ok(listing) => listing.installed,
        Err(error) => return report(&name, &[error.to_string()]),
    };
    let package = match installed
        .iter()
        .find(|package| config::short_name(&package.name) == call.provider)
    {
        Some(package) => package,
        None => {
            return report(
                &name,
                &[format!(
                    "call.provider: `{}` is not an installed package",
                    call.provider
                )],
            );
        }
    };
    let dir = home
        .join("extensions")
        .join(config::dir_name(&package.name));
    let host = extensions::HostScript::new(case.host.http, case.host.exec);
    let clock: Arc<dyn Clock> = CaseClock::new();
    let extension = Arc::new(
        extensions::LuaExtension::new(package.name.clone(), dir, home, Arc::clone(&clock))
            .with_host_script(Arc::clone(&host)),
    );
    let provider = extensions::LuaProvider::new(extension, call.provider);
    let key = arg.key.map(Secret::new);
    let result = provider.cost(&arg.generation_id, &arg.base_url, key.as_ref());
    let actual = match result {
        Ok(value) => Ok(value.map_or(Value::Null, |cost| json!(cost))),
        Err(error) => Err(json!({
            "code": error.code(),
            "message": error.to_string()
        })),
    };
    let mut failures = compare_result(case.returns.as_ref(), case.error.as_ref(), actual);
    failures.extend(
        host.unmet()
            .iter()
            .map(|entry| format!("unmet host call: {entry}")),
    );
    report(&name, &failures)
}

/// The only provider function this ticket supports for a direct case.
fn function(name: &str) -> Result<(), String> {
    if name == "cost" {
        Ok(())
    } else {
        Err(format!(
            "call.function: unsupported `{name}`; supported functions are `cost`"
        ))
    }
}

struct CostArgs {
    generation_id: GenerationId,
    base_url: String,
    key: Option<String>,
}

fn cost_args(value: &Value) -> Result<CostArgs, String> {
    let map = value
        .as_object()
        .ok_or("call.arg: cost expects an object")?;
    let generation_id = map
        .get("generation_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("call.arg.generation_id: expected a string")?;
    let base_url = map
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("call.arg.base_url: expected a string")?;
    let key = match map.get("key") {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(key.clone()),
        Some(_) => return Err("call.arg.key: expected a string or null".to_owned()),
    };
    Ok(CostArgs {
        generation_id: GenerationId(generation_id),
        base_url,
        key,
    })
}

/// Compares a provider result against exactly one expected case outcome.
fn compare_result(
    returns: Option<&Value>,
    error: Option<&Value>,
    actual: Result<Value, Value>,
) -> Vec<String> {
    match (returns, error, actual) {
        (Some(expected), None, Ok(actual)) => extensions::json_matches(expected, &actual)
            .err()
            .map(|reason| vec![format!("returns: {reason}")])
            .unwrap_or_default(),
        (None, Some(expected), Err(actual)) => extensions::json_matches(expected, &actual)
            .err()
            .map(|reason| vec![format!("error.{reason}")])
            .unwrap_or_default(),
        (Some(_), None, Err(actual)) => vec![format!("returns: provider call failed: {actual}")],
        (None, Some(_), Ok(actual)) => {
            vec![format!(
                "error: expected a provider error, returned {actual}"
            )]
        }
        (Some(_), Some(_), _) | (None, None, _) => {
            vec!["case must expect exactly one of returns or error".to_owned()]
        }
    }
}

#[cfg(test)]
#[path = "call_tests.rs"]
mod tests;
