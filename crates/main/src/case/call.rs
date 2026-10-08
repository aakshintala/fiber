//! Direct calls to a package's Lua provider (`docs/testing.md`, "Testing an extension").

use std::path::Path;
use std::sync::Arc;

use config::Secret;
use contract::GenerationId;
use contract::clock::Clock;
use serde_json::{Value, json};

use super::clock::CaseClock;
use super::format::{CallCase, CallOutcome};
use super::run::{malformed, report};

/// Runs a provider call without creating a session.
pub(crate) fn run_case(case: CallCase, name: String, process_clock: Arc<dyn Clock>) -> i32 {
    let call = case.call;
    if let Err(error) = function(&call.function) {
        return malformed(&name, &[error]);
    }
    let arg = match cost_args(&call.arg) {
        Ok(arg) => arg,
        Err(error) => return malformed(&name, &[error]),
    };
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return report(&name, &[error.to_string()]),
    };
    let installed = match extensions::list(&home, process_clock.as_ref()) {
        Ok(listing) => listing.installed,
        Err(error) => return report(&name, &[error.to_string()]),
    };
    let host = extensions::HostScript::new(case.host.http, case.host.exec);
    let clock: Arc<dyn Clock> = CaseClock::new();
    let package = match provider_package(&installed, &call.provider, &home, &clock, &host) {
        Some(package) => package,
        None => {
            return report(
                &name,
                &[format!(
                    "call.provider: no installed package registers provider `{}`",
                    call.provider
                )],
            );
        }
    };
    let dir = home
        .join("extensions")
        .join(config::dir_name(&package.name));
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
    let mut failures = compare_result(&case.outcome, actual);
    failures.extend(
        host.unmet()
            .iter()
            .map(|entry| format!("unmet host call: {entry}")),
    );
    report(&name, &failures)
}

/// The installed package a call case loads: the one whose short name is
/// the provider, else the one that registers it. A package's registered
/// provider name can differ from its own name, and the case runs inside
/// the package under test (`docs/testing.md`, "Testing an extension").
fn provider_package<'a>(
    installed: &'a [extensions::Installed],
    provider: &str,
    home: &Path,
    clock: &Arc<dyn Clock>,
    host: &Arc<extensions::HostScript>,
) -> Option<&'a extensions::Installed> {
    if let Some(package) = installed
        .iter()
        .find(|package| config::short_name(&package.name) == provider)
    {
        return Some(package);
    }
    installed.iter().find(|package| {
        let dir = home
            .join("extensions")
            .join(config::dir_name(&package.name));
        let extension = extensions::LuaExtension::new(
            package.name.clone(),
            dir,
            home.to_path_buf(),
            Arc::clone(clock),
        )
        .with_host_script(Arc::clone(host));
        extension
            .provider_names()
            .is_ok_and(|names| names.iter().any(|name| name == provider))
    })
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

/// Compares a provider result against the call case's one expected outcome.
fn compare_result(outcome: &CallOutcome, actual: Result<Value, Value>) -> Vec<String> {
    match (outcome, actual) {
        (CallOutcome::Returns(expected), Ok(actual)) => extensions::json_matches(expected, &actual)
            .err()
            .map(|reason| vec![format!("returns: {reason}")])
            .unwrap_or_default(),
        (CallOutcome::Error(expected), Err(actual)) => extensions::json_matches(expected, &actual)
            .err()
            .map(|reason| vec![format!("error.{reason}")])
            .unwrap_or_default(),
        (CallOutcome::Returns(_), Err(actual)) => {
            vec![format!("returns: provider call failed: {actual}")]
        }
        (CallOutcome::Error(_), Ok(actual)) => {
            vec![format!(
                "error: expected a provider error, returned {actual}"
            )]
        }
    }
}

#[cfg(test)]
#[path = "call_tests.rs"]
mod tests;
