//! Direct calls to a package's Lua provider (`docs/testing.md`, "Testing an extension").

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::Secret;
use contract::GenerationId;
use contract::clock::Clock;
use serde_json::{Value, json};
use support::clock::{Manual, Sleep};

use super::Verdict;
use super::format::{Call, CallCase, CallOperation, CallOutcome};

/// Runs a provider call without creating a session and returns its verdict.
pub fn run_call(case: CallCase, process_clock: Arc<dyn Clock>) -> Verdict {
    let mut provider_name = None;
    for step in &case.steps {
        if let CallOperation::Invoke { call, .. } = &step.operation {
            if let Err(error) = ready(call) {
                return malformed_verdict(vec![error]);
            }
            if provider_name
                .as_ref()
                .is_some_and(|name| name != &call.provider)
            {
                return malformed_verdict(vec![
                    "calls: all calls must use one provider".to_owned(),
                ]);
            }
            provider_name = Some(call.provider.clone());
        }
    }
    let Some(provider_name) = provider_name else {
        return malformed_verdict(vec![
            "calls: at least one provider call is required".to_owned(),
        ]);
    };
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return failure_verdict(vec![error.to_string()]),
    };
    let installed = match crate::list(&home, process_clock.as_ref()) {
        Ok(listing) => listing.installed,
        Err(error) => return failure_verdict(vec![error.to_string()]),
    };
    if let Err(error) = write_credentials(&home, &case.credentials) {
        return failure_verdict(vec![error]);
    }
    let data = match crate::Providers::load(&home) {
        Ok((providers, _)) => providers.data(&provider_name),
        Err(error) => return failure_verdict(vec![error.to_string()]),
    };
    let host = crate::HostScript::new(case.host.http, case.host.exec, case.host.oauth);
    let case_clock = Manual::with_sleep(Sleep::Waits);
    let clock: Arc<dyn Clock> = case_clock.clone();
    let extension = match provider_extension(
        &installed,
        &provider_name,
        &home,
        &clock,
        &host,
        case.attended,
    ) {
        Some(extension) => extension,
        None => {
            return failure_verdict(vec![format!(
                "call.provider: no installed package registers provider `{}`",
                provider_name
            )]);
        }
    };
    let provider = crate::LuaProvider::new(extension, provider_name);
    let mut pair = crate::CredentialPair::for_provider(&data, "default");
    let mut failures = Vec::new();
    let mut advances = Some(case.clock);
    for (index, step) in case.steps.into_iter().enumerate() {
        for advance in step.clock {
            #[allow(
                clippy::disallowed_methods,
                reason = "CaseClock scripted advance; excluded from the FakeClock ban (rulings 6253642596, 6261178927)"
            )]
            case_clock.advance(Duration::from_millis(advance.advance_ms));
        }
        match step.operation {
            CallOperation::AwaitCredentialIdle => {
                if !provider.await_idle(&pair, CALL_WAIT) {
                    failures.push(format!(
                        "calls[{}].await: credential_idle expired",
                        index.saturating_add(1)
                    ));
                }
            }
            CallOperation::Invoke { call, outcome } => {
                let ready = match ready(&call) {
                    Ok(ready) => ready,
                    Err(error) => {
                        failures.push(error);
                        break;
                    }
                };
                if !matches!(ready, Ready::Sign { .. }) {
                    pair.label = ready.label().unwrap_or("default").to_owned();
                }
                let owned_provider = Arc::clone(&provider);
                let mut owned_pair = pair.clone();
                let owned_home = home.clone();
                let (sent, received) = mpsc::channel();
                let worker = match std::thread::Builder::new()
                    .name("fiber-case-call".to_owned())
                    .spawn(move || {
                        let result = invoke(&owned_provider, &mut owned_pair, &owned_home, ready);
                        drop(sent.send((result, owned_pair)));
                    }) {
                    Ok(worker) => worker,
                    Err(error) => {
                        failures.push(format!("starting provider call: {error}"));
                        break;
                    }
                };
                for advance in advances.take().unwrap_or_default() {
                    if let Err(error) = case_clock.advance_when_parked_in(
                        Duration::from_millis(advance.advance_ms),
                        super::run::ADVANCE_WAIT,
                    ) {
                        failures.push(format!("clock: {error}"));
                        provider.stop();
                        break;
                    }
                }
                #[allow(
                    clippy::disallowed_methods,
                    reason = "the case runner's bounded wait for a provider call on a real channel"
                )]
                let received = received.recv_timeout(CALL_WAIT);
                match received {
                    Ok((actual, actual_pair)) => {
                        pair = actual_pair;
                        failures.extend(compare_result(&outcome, actual).into_iter().map(
                            |failure| format!("calls[{}].{failure}", index.saturating_add(1)),
                        ));
                    }
                    Err(_) => {
                        failures.push(
                            "provider call did not finish within the runner bound".to_owned(),
                        );
                        provider.stop();
                    }
                }
                // Stopping abandons a stuck VM and releases every provider caller.
                if worker.join().is_err() {
                    failures.push("provider call thread failed".to_owned());
                }
            }
        }
        if !failures.is_empty() {
            break;
        }
    }
    if !provider.await_idle(&pair, CALL_WAIT) {
        failures.push("credential fetch did not finish within the runner bound".to_owned());
    }
    provider.stop();
    failures.extend(check_credentials(
        &home,
        &case.expect_credentials,
        case.exact_credentials,
    ));
    failures.extend(
        host.unmet()
            .iter()
            .map(|entry| format!("unmet host call: {entry}")),
    );
    failure_verdict(failures)
}

fn failure_verdict(failures: Vec<String>) -> Verdict {
    if failures.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail(failures)
    }
}

fn malformed_verdict(failures: Vec<String>) -> Verdict {
    Verdict::Malformed(failures)
}

const CALL_WAIT: Duration = super::run::UNTIL_WAIT;

fn invoke(
    provider: &Arc<crate::LuaProvider>,
    pair: &mut crate::CredentialPair,
    home: &Path,
    ready: Ready,
) -> Result<Value, Value> {
    match ready {
        Ready::Cost(arg) => {
            let key = arg.key.map(Secret::new);
            match provider.cost(&arg.generation_id, &arg.base_url, key.as_ref()) {
                Ok(value) => Ok(value.map_or(Value::Null, |cost| json!(cost))),
                Err(error) => Err(json!({
                    "code": error.code(),
                    "message": error.to_string()
                })),
            }
        }
        Ready::Models(arg) => match provider.list_models(&arg) {
            Ok((_, returned)) => Ok(returned),
            Err(error) => Err(json!({
                "code": error.code(),
                "message": error.to_string()
            })),
        },
        Ready::Quota { key, .. } => provider
            .fetch_quota(pair, key.map(Secret::new).as_ref())
            .map(|(_, returned)| returned)
            .map_err(extension_error),
        Ready::Credential(_) => provider.credential_value(pair).map_err(extension_error),
        Ready::Login { method, label } => {
            let logged = provider
                .login(&pair.credential, label.as_deref(), method)
                .map_err(extension_error)?;
            let value = logged.stored.as_value().clone();
            let label = label
                .as_deref()
                .or(logged.email.as_deref())
                .unwrap_or("default");
            pair.label = label.to_owned();
            let file = config::CredentialFile::new(home, &pair.credential, label)
                .map_err(|error| json!({"code": error.code(), "message": error.to_string()}))?;
            let held = file
                .try_lock()
                .map_err(|error| json!({"code": error.code(), "message": error.to_string()}))?
                .ok_or_else(
                    || json!({"code": "io_failed", "message": "credential file is locked"}),
                )?;
            held.write(&value)
                .map_err(|error| json!({"code": error.code(), "message": error.to_string()}))?;
            Ok(value)
        }
        Ready::Sign {
            method,
            url,
            headers,
        } => {
            let signer = provider.signer(pair.clone()).map_err(extension_error)?;
            let Some(signer) = signer else {
                return Ok(json!({}));
            };
            let signed = signer
                .sign(&contract::signing::SignRequest {
                    method: &method,
                    url: &url,
                    headers: &headers,
                    body: &[],
                })
                .map_err(|error| {
                    let code = match &error {
                        contract::signing::Error::Credential { code, .. } => code.clone(),
                        contract::signing::Error::Unattended { .. }
                        | contract::signing::Error::LoginTimedOut { .. } => {
                            contract::ErrorCode::AuthenticationFailed
                        }
                        contract::signing::Error::Failed(_)
                        | contract::signing::Error::NotHeaders(_) => {
                            contract::ErrorCode::CredentialFailed
                        }
                    };
                    json!({"code": code, "message": error.to_string()})
                })?;
            Ok(Value::Object(
                signed
                    .into_iter()
                    .map(|(key, value)| (key, Value::String(value)))
                    .collect(),
            ))
        }
    }
}

fn extension_error(error: crate::Error) -> Value {
    json!({"code": error.code(), "message": error.to_string()})
}

fn write_credentials(home: &Path, values: &serde_json::Map<String, Value>) -> Result<(), String> {
    for (key, value) in values {
        let (credential, label) = key
            .split_once('/')
            .ok_or("credentials: expected credential/label")?;
        let file = config::CredentialFile::new(home, credential, label)
            .map_err(|error| error.to_string())?;
        let held = file
            .try_lock()
            .map_err(|error| error.to_string())?
            .ok_or("credentials: file is locked")?;
        held.write(value).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn check_credentials(
    home: &Path,
    expected: &serde_json::Map<String, Value>,
    exact: bool,
) -> Vec<String> {
    expected
        .iter()
        .filter_map(|(key, value)| {
            let result = (|| {
                let (credential, label) = key
                    .split_once('/')
                    .ok_or("expected credential/label".to_owned())?;
                let file = config::CredentialFile::new(home, credential, label)
                    .map_err(|error| error.to_string())?;
                let held = file
                    .try_lock()
                    .map_err(|error| error.to_string())?
                    .ok_or("file is locked".to_owned())?;
                let actual = held
                    .read()
                    .map_err(|error| error.to_string())?
                    .unwrap_or(Value::Null);
                compare_credential(value, &actual, exact)
            })();
            result
                .err()
                .map(|error| format!("expect_credentials.{key}: {error}"))
        })
        .collect()
}

fn compare_credential(expected: &Value, actual: &Value, exact: bool) -> Result<(), String> {
    if exact && !same_json_shape(expected, actual) {
        return Err("stored value differs from the expected whole value".to_owned());
    }
    crate::json_matches(expected, actual)
}

fn same_json_shape(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, value)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| same_json_shape(value, actual))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| same_json_shape(expected, actual)),
        _ => true,
    }
}

struct CaseBrowser(bool);
impl crate::Browser for CaseBrowser {
    fn open(&self, _: &str) {}
    fn show(&self, _: &str, _: &str) {}
    fn attended(&self) -> bool {
        self.0
    }
}

/// The installed package a call case loads: the one whose short name is
/// the provider, else the one that registers it. A package's registered
/// provider name can differ from its own name, and the case runs inside
/// the package under test (`docs/testing.md`, "Testing an extension").
fn provider_extension(
    installed: &[crate::Installed],
    provider: &str,
    home: &Path,
    clock: &Arc<dyn Clock>,
    host: &Arc<crate::HostScript>,
    attended: bool,
) -> Option<Arc<crate::LuaExtension>> {
    let extension_for = |package: &crate::Installed| {
        let dir = config::extensions_dir(home).join(config::dir_name(&package.name));
        let deps = crate::dep_dirs(home, &dir, &package.depends, false);
        Arc::new(
            crate::LuaExtension::new(
                package.name.clone(),
                dir,
                home.to_path_buf(),
                Arc::clone(clock),
            )
            .with_dependencies(deps)
            .with_host_script(Arc::clone(host))
            .with_browser(Arc::new(CaseBrowser(attended))),
        )
    };
    if let Some(package) = installed
        .iter()
        .find(|package| config::short_name(&package.name) == provider)
    {
        return Some(extension_for(package));
    }
    installed.iter().find_map(|package| {
        let dir = config::extensions_dir(home).join(config::dir_name(&package.name));
        let manifest = config::read_manifest(&dir).ok()?;
        (manifest.providers.contains_key(provider)
            && dir
                .join("providers")
                .join(format!("{provider}.lua"))
                .is_file())
        .then(|| extension_for(package))
    })
}

/// A validated provider call: `cost`, `quota` and `models` carry their
/// args.
#[derive(Debug)]
enum Ready {
    Cost(CostArgs),
    Models(crate::ModelsArg),
    Quota {
        label: Option<String>,
        key: Option<String>,
    },
    Credential(Option<String>),
    Login {
        method: crate::LoginMethod,
        label: Option<String>,
    },
    Sign {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
    },
}

impl Ready {
    fn label(&self) -> Option<&str> {
        match self {
            Ready::Credential(label) | Ready::Login { label, .. } => label.as_deref(),
            Ready::Quota { label, .. } => label.as_deref(),
            Ready::Cost(_) | Ready::Models(_) | Ready::Sign { .. } => None,
        }
    }
}

/// Validates `call`'s function and args. Anything but `cost`, `models`
/// and `quota` names every supported function.
fn ready(call: &Call) -> Result<Ready, String> {
    match call.function.as_str() {
        "cost" => cost_args(&call.arg).map(Ready::Cost),
        "models" => models_args(&call.arg).map(Ready::Models),
        "quota" => quota_args(&call.arg),
        "credential" | "login" | "sign" => credential_args(call),
        _ => Err(format!(
            "call.function: unsupported `{}`; supported functions are `cost`, `models`, `quota`, `login`, `credential` and `sign`",
            call.function
        )),
    }
}

fn credential_args(call: &Call) -> Result<Ready, String> {
    let map = call.arg.as_object().ok_or("call.arg: expected an object")?;
    let label = match map.get("label") {
        None => None,
        Some(Value::String(label)) => Some(label.clone()),
        Some(_) => return Err("call.arg.label: expected a string".to_owned()),
    };
    match call.function.as_str() {
        "credential" => {
            if map.keys().any(|key| key != "label") {
                return Err("call.arg: credential takes only label".to_owned());
            }
            Ok(Ready::Credential(label))
        }
        "login" => {
            if map
                .keys()
                .any(|key| !["method", "label"].contains(&key.as_str()))
            {
                return Err("call.arg: login takes method and label".to_owned());
            }
            let method = match map.get("method").and_then(Value::as_str) {
                Some("browser") => crate::LoginMethod::Browser,
                Some("device") => crate::LoginMethod::Device,
                _ => return Err("call.arg.method: expected browser or device".to_owned()),
            };
            Ok(Ready::Login { method, label })
        }
        "sign" => {
            if map
                .keys()
                .any(|key| !["method", "url", "headers"].contains(&key.as_str()))
            {
                return Err("call.arg: sign takes method, url and headers".to_owned());
            }
            let method = map
                .get("method")
                .and_then(Value::as_str)
                .ok_or("call.arg.method: expected a string")?
                .to_owned();
            let url = map
                .get("url")
                .and_then(Value::as_str)
                .ok_or("call.arg.url: expected a string")?
                .to_owned();
            let headers = map
                .get("headers")
                .and_then(Value::as_object)
                .ok_or("call.arg.headers: expected an object")?
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.clone(), value.to_owned()))
                        .ok_or("call.arg.headers: expected string values".to_owned())
                })
                .collect::<Result<_, _>>()?;
            Ok(Ready::Sign {
                method,
                url,
                headers,
            })
        }
        _ => Err("call.function: unsupported credential function".to_owned()),
    }
}

/// `models` takes an object with only an optional `key` and
/// `placeholders`, as `quota` takes `key`: what `models()` receives. A
/// null key is no key; `placeholders` maps each name to its host.
fn models_args(value: &Value) -> Result<crate::ModelsArg, String> {
    let map = value
        .as_object()
        .ok_or("call.arg: models expects an object")?;
    if map
        .keys()
        .any(|key| !["key", "placeholders"].contains(&key.as_str()))
    {
        return Err("call.arg: models takes only key and placeholders".to_owned());
    }
    let key = match map.get("key") {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(Secret::new(key.clone())),
        Some(_) => return Err("call.arg.key: expected a string or null".to_owned()),
    };
    let placeholders = match map.get("placeholders") {
        None => BTreeMap::new(),
        Some(Value::Object(placeholders)) => {
            let mut hosts = BTreeMap::new();
            for (name, host) in placeholders {
                let Some(host) = host.as_str() else {
                    return Err("call.arg.placeholders: expected an object of strings".to_owned());
                };
                hosts.insert(name.clone(), host.to_owned());
            }
            hosts
        }
        Some(_) => {
            return Err("call.arg.placeholders: expected an object of strings".to_owned());
        }
    };
    Ok(crate::ModelsArg { key, placeholders })
}

/// `quota` takes an object with only an optional `label` and `key`, as
/// `credential` takes `label` and `cost` takes `key`. The label defaults
/// to `default` at the call site; a null key is no key.
fn quota_args(value: &Value) -> Result<Ready, String> {
    let map = value
        .as_object()
        .ok_or("call.arg: quota expects an object")?;
    if map
        .keys()
        .any(|key| !["label", "key"].contains(&key.as_str()))
    {
        return Err("call.arg: quota takes only label and key".to_owned());
    }
    let label = match map.get("label") {
        None => None,
        Some(Value::String(label)) => Some(label.clone()),
        Some(_) => return Err("call.arg.label: expected a string".to_owned()),
    };
    let key = match map.get("key") {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(key.clone()),
        Some(_) => return Err("call.arg.key: expected a string or null".to_owned()),
    };
    Ok(Ready::Quota { label, key })
}

#[derive(Debug)]
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
        (CallOutcome::Returns(expected), Ok(actual)) => crate::json_matches(expected, &actual)
            .err()
            .map(|reason| vec![format!("returns: {reason}")])
            .unwrap_or_default(),
        (CallOutcome::Error(expected), Err(actual)) => crate::json_matches(expected, &actual)
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
