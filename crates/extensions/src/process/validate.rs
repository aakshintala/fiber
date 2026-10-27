//! What the wire's JSON registration calls check (`super`): each
//! `fiber.*` call validated exactly as its Lua twin validates it, in its
//! order, without touching registry state. A validator reads only its
//! `args` and the explicit context the registry passes (an index, a name
//! set, a manifest slice); the registry owns every state read, every
//! reference mint, and every insertion.
//!
//! This mirrors the Lua side, where `read` validates one call into a
//! declaration (or a refusal sentence) and the registry inserts it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{Map, Value};

use contract::ErrorCode;
use contract::tool::Bound;

use crate::Error;
use crate::lua::{
    DeclaredHook, DeclaredTool, DeclaredWatcher, HookPhase, StaticEffects, effects_from,
    is_known_kind,
};

/// What a validator that can also fail its package answers: the validated
/// declaration, a refusal sentence for the notice, or the package failure.
pub(crate) enum Checked<T> {
    /// The call validates; the registry mints and inserts.
    Accept(T),
    /// The call does not stand; the sentence rides the notice.
    Refuse(String),
    /// The package is out; the sentence latches as its failure.
    Fail(ErrorCode, String),
}

/// A validated `fiber.tool` call: the declaration to insert, and whether
/// its `effects` is a function needing its own reference.
pub(crate) struct ValidTool {
    /// The tool's declaration, as `fiber.tool` validated it.
    pub(crate) tool: DeclaredTool,
    /// Whether `effects` is a function, minted as an `effects` reference.
    pub(crate) effects_fn: bool,
}

/// A validated `fiber.hook` call: the hook's point and declaration. The
/// registry numbers the hook among its point's hooks at insertion.
pub(crate) struct ValidHook {
    /// The hook point, such as `turn_end`.
    pub(crate) point: String,
    /// The hook's declaration, as `fiber.hook` validated it.
    pub(crate) hook: DeclaredHook,
}

/// A validated `fiber.watch` call: the watcher to insert, numbered by the
/// registry's index at insertion.
pub(crate) struct ValidWatch {
    /// The watcher's declaration, with only known kinds kept.
    pub(crate) watcher: DeclaredWatcher,
}

/// What a `fiber.watch` validation leaves: every unknown kind's notice,
/// and either the watcher to insert, no watcher with the call still
/// standing, or the refusal sentence.
pub(crate) struct WatchChecked {
    /// Every unknown kind's notice, kept whether or not a watcher stands.
    pub(crate) notices: Vec<String>,
    /// The watcher to insert, none with the call still standing, or the
    /// refusal sentence.
    pub(crate) result: Result<Option<ValidWatch>, String>,
}

/// A validated `fiber.command` call: the entry to insert under its name.
/// The registry checks the manifest after this, and a later call for the
/// same name replaces the first, as in Lua.
pub(crate) struct ValidCommand {
    /// The command's registered name.
    pub(crate) name: String,
    /// The command's description, as the session lists it.
    pub(crate) description: String,
    /// The command's timeout, enforced when the command runs.
    pub(crate) timeout: Duration,
}

/// A validated `fiber.search_backend` call: the backend to insert.
pub(crate) struct ValidSearch {
    /// The backend's name.
    pub(crate) name: String,
    /// The backend's timeout, enforced when the backend runs.
    pub(crate) timeout: Duration,
}

/// A validated `fiber.provider` call: the timeouts and function names to
/// insert. The registry mints one reference per function.
pub(crate) struct ValidProvider {
    /// The provider's name.
    pub(crate) name: String,
    /// Each function's timeout, by function name.
    pub(crate) timeouts: BTreeMap<String, Duration>,
    /// Each function's name, minted as its reference.
    pub(crate) funcs: Vec<String>,
}

/// A validated `fiber.harness` call: the entry to insert. The registry
/// mints one reference per function in `funcs`.
pub(crate) struct ValidHarness {
    /// The harness's name.
    pub(crate) name: String,
    /// The timeout every harness function runs under.
    pub(crate) timeout: Duration,
    /// The harness's data, carried for the delegate runner.
    pub(crate) data: BTreeMap<String, Value>,
    /// Each function's name: `command`, `line`, `models` and `quota` when
    /// declared as a function.
    pub(crate) funcs: Vec<String>,
}

/// The `args` value at `index`, when the call passed one.
fn arg(args: &[Value], index: usize) -> Option<&Value> {
    args.get(index)
}

/// How Lua shows a registration name that is not a string: the value, or
/// `nil` for nothing and `table` for what has no short rendering. JSON
/// `null` reads as Lua's `nil`, which is what the wire decodes it to.
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "nil".to_owned(),
        Some(Value::String(name)) => name.clone(),
        Some(Value::Bool(held)) => held.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(_) => "table".to_owned(),
    }
}

/// A whole number of milliseconds above 0, as every callback declares.
fn millis(value: Option<&Value>) -> Option<u64> {
    value?.as_u64().filter(|millis| *millis > 0)
}

/// Whether `name` is 1 to 128 of `A-Z`, `a-z`, `0-9`, `_` and `-`: what a
/// tool may be called.
fn is_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `name` is plain: non-empty with no whitespace, `/` or `:`.
fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && !name.contains('/')
        && !name.contains(':')
}

/// Whether `point` is a hook point.
fn is_hook_point(point: &str) -> bool {
    matches!(
        point,
        "session_start"
            | "before_message"
            | "turn_start"
            | "before_tool"
            | "before_model_call"
            | "after_tool"
            | "turn_end"
            | "before_handoff"
    )
}

/// A tool's `keep`: the byte counts of the start and of the end of a long
/// result, exactly as `fiber.tool` reads them.
fn keep_of(keep: &Map<String, Value>) -> Result<Bound, String> {
    for key in keep.keys() {
        if key != "head" && key != "tail" {
            return Err(format!("`keep`: `{key}` is not `head` or `tail`"));
        }
    }
    let pair = |key: &str| -> Result<usize, String> {
        match keep.get(key) {
            None | Some(Value::Null) => Ok(0),
            Some(Value::Number(number)) => number
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("`keep`: `{key}` must be a whole number of bytes")),
            _ => Err(format!("`keep`: `{key}` must be a whole number of bytes")),
        }
    };
    let head = pair("head")?;
    let tail = pair("tail")?;
    if head == 0 && tail == 0 {
        return Err("`keep`: `head` and `tail` cannot both be 0".to_owned());
    }
    head.checked_add(tail)
        .map(|_| Bound {
            start: head,
            end: tail,
        })
        .ok_or_else(|| "`keep`: `head` plus `tail` is too large".to_owned())
}

/// Validates one `fiber.tool` call's `args` into its declaration: exactly
/// the checks `fiber.tool` runs, in its order, over JSON. The registry
/// refuses a duplicate name after this and mints the references.
pub(crate) fn check_tool(args: &[Value]) -> Result<ValidTool, String> {
    let name = arg(args, 0);
    let spec = arg(args, 1);
    let fail = |why: &str| format!("`{}` tool not registered: {why}", shown(name));
    let Some(Value::String(name)) = name else {
        return Err(fail("the name must be a string"));
    };
    if !is_tool_name(name) {
        return Err(fail(
            "a name is 1 to 128 of `A-Z`, `a-z`, `0-9`, `_` and `-`",
        ));
    }
    let Some(Value::Object(spec)) = spec else {
        return Err(fail(
            "it takes a table of `description`, `input_schema`, `effects`, `timeout`, `run` and optionally `keep`",
        ));
    };
    let description = match spec.get("description") {
        Some(Value::String(description)) if !description.is_empty() => description.clone(),
        _ => {
            return Err(fail("`description` must be a non-empty string"));
        }
    };
    let input_schema = match spec.get("input_schema") {
        Some(Value::Object(schema))
            if schema.get("type") == Some(&Value::String("object".to_owned())) =>
        {
            Value::Object(schema.clone())
        }
        _ => {
            return Err(fail(
                "`input_schema` must be a JSON object whose `type` is `object`",
            ));
        }
    };
    let (effects, effects_fn) = match spec.get("effects") {
        None | Some(Value::Null) => return Err(fail("missing `effects`")),
        Some(Value::Bool(true)) => (None, true),
        Some(Value::Object(effects)) => match effects_from(&Value::Object(effects.clone())) {
            Ok((declared, hosts)) => (Some(StaticEffects { declared, hosts }), false),
            Err(why) => return Err(fail(&format!("`effects`: {why}"))),
        },
        _ => {
            return Err(fail("`effects` must be a table or a function"));
        }
    };
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => return Err(fail("missing `timeout`")),
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0",
                ));
            }
        },
    };
    if spec.get("run") != Some(&Value::Bool(true)) {
        return Err(fail("`run` must be a function"));
    }
    let keep = match spec.get("keep") {
        None | Some(Value::Null) => Bound::DEFAULT,
        Some(Value::Object(keep)) => match keep_of(keep) {
            Ok(keep) => keep,
            Err(why) => return Err(fail(&why)),
        },
        _ => {
            return Err(fail("`keep` must be a table of `head` and `tail`"));
        }
    };
    Ok(ValidTool {
        tool: DeclaredTool {
            name: name.clone(),
            description,
            input_schema,
            effects,
            timeout,
            keep,
        },
        effects_fn,
    })
}

/// Validates one `fiber.hook` call's `args` into its point and
/// declaration: exactly the checks `fiber.hook` runs, in its order, over
/// JSON. The registry numbers the hook among its point's hooks and mints
/// its reference.
pub(crate) fn check_hook(args: &[Value]) -> Result<ValidHook, String> {
    let point = arg(args, 0);
    let spec = arg(args, 1);
    let entry = match spec {
        Some(Value::Object(spec)) => match spec.get("entry") {
            Some(Value::String(entry)) => Some(entry.clone()),
            _ => None,
        },
        _ => None,
    };
    let fail = |shown: &str, why: &str| match &entry {
        Some(entry) => {
            format!("`{shown}` hook not registered for entry `{entry}`: {why}")
        }
        None => format!("`{shown}` hook not registered: {why}"),
    };
    let point_text = shown(point);
    let Some(Value::String(point)) = point else {
        return Err(fail(
            &point_text,
            &format!("`{point_text}` is not a hook point"),
        ));
    };
    if !is_hook_point(point) {
        return Err(fail(point, &format!("`{point}` is not a hook point")));
    }
    let Some(Value::Object(spec)) = spec else {
        return Err(fail(
            point,
            "it takes a table of `timeout`, `on_failure` and `run`",
        ));
    };
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => {
            return Err(fail(point, "missing `timeout`"));
        }
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return Err(fail(
                    point,
                    "`timeout` must be a whole number of milliseconds above 0",
                ));
            }
        },
    };
    let blocking = match spec.get("on_failure") {
        None | Some(Value::Null) => {
            return Err(fail(point, "missing `on_failure`"));
        }
        Some(Value::String(failure)) if failure == "blocking" => true,
        Some(Value::String(failure)) if failure == "non-blocking" => false,
        _ => {
            return Err(fail(
                point,
                "`on_failure` must be `blocking` or `non-blocking`",
            ));
        }
    };
    let (phase, phase_explicit) = match spec.get("phase") {
        None | Some(Value::Null) => (HookPhase::Transform, false),
        Some(Value::String(phase)) if phase == "sanitize" => (HookPhase::Sanitize, true),
        Some(Value::String(phase)) if phase == "transform" => (HookPhase::Transform, true),
        Some(Value::String(phase)) if phase == "check" => (HookPhase::Check, true),
        _ => {
            return Err(fail(
                point,
                "`phase` must be `sanitize`, `transform` or `check`",
            ));
        }
    };
    if phase == HookPhase::Check
        && !matches!(
            point.as_str(),
            "before_message" | "before_tool" | "before_model_call"
        )
    {
        return Err(fail(
            point,
            "a `check` hook exists only at `before_message`, `before_tool` and `before_model_call`",
        ));
    }
    if spec.get("run") != Some(&Value::Bool(true)) {
        return Err(fail(point, "`run` must be a function"));
    }
    // The declared configuration key rides only the first-party hooks
    // configuration registrar: an ordinary process registration that
    // supplies one is refused.
    if spec
        .get("entry")
        .is_some_and(|entry| !matches!(entry, Value::Null))
    {
        return Err(fail(point, "`entry` is declared configuration only"));
    }
    Ok(ValidHook {
        point: point.clone(),
        hook: DeclaredHook {
            phase,
            timeout,
            blocking,
            entry: None,
            phase_explicit,
        },
    })
}

/// Validates one `fiber.watch` call's `args`: exactly the checks
/// `fiber.watch` runs, with kinds checked against the event kinds, as
/// `DeclaredWatchers` does. `index` numbers the attempt, as Lua numbers
/// each attempt by the watchers registered so far: a refused call takes
/// no number. The registry records the notices and inserts the watcher,
/// if any.
pub(crate) fn check_watch(args: &[Value], index: usize) -> WatchChecked {
    let fail = |why: &str| format!("`watcher:{index}` watcher not registered: {why}");
    let kinds = match arg(args, 0) {
        Some(Value::Array(kinds))
            if !kinds.is_empty()
                && kinds
                    .iter()
                    .all(|kind| matches!(kind, Value::String(kind) if !kind.is_empty())) =>
        {
            kinds
                .iter()
                .filter_map(|kind| kind.as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        }
        _ => {
            return WatchChecked {
                notices: Vec::new(),
                result: Err(fail("`kinds` must be a list of event kinds")),
            };
        }
    };
    let spec = match arg(args, 1) {
        Some(Value::Object(spec)) => spec,
        _ => {
            return WatchChecked {
                notices: Vec::new(),
                result: Err(fail("it takes a table of `timeout` and `run`")),
            };
        }
    };
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => {
            return WatchChecked {
                notices: Vec::new(),
                result: Err(fail("missing `timeout`")),
            };
        }
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return WatchChecked {
                    notices: Vec::new(),
                    result: Err(fail(
                        "`timeout` must be a whole number of milliseconds above 0",
                    )),
                };
            }
        },
    };
    if spec.get("run") != Some(&Value::Bool(true)) {
        return WatchChecked {
            notices: Vec::new(),
            result: Err(fail("`run` must be a function")),
        };
    }
    // The declared configuration key rides only the first-party hooks
    // configuration registrar: an ordinary process registration that
    // supplies one is refused.
    if spec
        .get("entry")
        .is_some_and(|entry| !matches!(entry, Value::Null))
    {
        return WatchChecked {
            notices: Vec::new(),
            result: Err(fail("`entry` is declared configuration only")),
        };
    }
    let mut wanted = Vec::new();
    let mut notices = Vec::new();
    for kind in &kinds {
        if is_known_kind(kind) {
            wanted.push(kind.clone());
        } else {
            notices.push(format!(
                "`watcher:{index}` watcher not registered: `{kind}` is not an event kind"
            ));
        }
    }
    if wanted.is_empty() {
        // No surviving kind registers nothing, as in Lua
        // (`DeclaredWatchers::read` keeps the problems and skips the
        // entry): the call still stands, and the notices ride with it.
        return WatchChecked {
            notices,
            result: Ok(None),
        };
    }
    WatchChecked {
        notices,
        result: Ok(Some(ValidWatch {
            watcher: DeclaredWatcher {
                kinds: wanted,
                timeout,
                entry: None,
            },
        })),
    }
}

/// Validates one `fiber.command` call's `args`: exactly the checks
/// `fiber.command` runs, in its order, over JSON. The registry checks the
/// manifest after this, and a later call for the same name replaces the
/// first, as in Lua.
pub(crate) fn check_command(args: &[Value]) -> Result<ValidCommand, String> {
    let name = arg(args, 0);
    let spec = arg(args, 1);
    let fail = |why: &str| format!("`{}` command not registered: {why}", shown(name));
    let Some(Value::String(name)) = name else {
        return Err(fail("the name must be a string"));
    };
    if name.is_empty()
        || name.chars().any(char::is_whitespace)
        || name.contains('/')
        || name.contains(':')
    {
        return Err(fail(&format!(
            "names are plain: `{name}` is empty or holds whitespace, `/` or `:`"
        )));
    }
    let Some(Value::Object(spec)) = spec else {
        return Err(fail(
            "it takes a table of `timeout`, `run` and `description`",
        ));
    };
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => return Err(fail("missing `timeout`")),
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0",
                ));
            }
        },
    };
    if spec.get("run") != Some(&Value::Bool(true)) {
        return Err(fail("`run` must be a function"));
    }
    let description = match spec.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(description)) if !description.contains('\n') => description.clone(),
        _ => {
            return Err(fail("`description` must be a single-line string"));
        }
    };
    Ok(ValidCommand {
        name: name.clone(),
        description,
        timeout,
    })
}

/// Validates one `fiber.search_backend` call's `args`: exactly the checks
/// `fiber.search_backend` runs, in its order, over JSON. Unlike commands,
/// a backend name need not be plain. `registered` names the backends
/// standing, refused before the spec reads, as the registry holds them.
pub(crate) fn check_search(
    args: &[Value],
    registered: &BTreeSet<String>,
) -> Result<ValidSearch, String> {
    let name = arg(args, 0);
    let spec = arg(args, 1);
    let fail = |why: &str| format!("`{}` search backend not registered: {why}", shown(name));
    let Some(Value::String(name)) = name else {
        return Err(fail("the name must be a string"));
    };
    if registered.contains(name) {
        return Err(format!("`{name}` search backend is already registered"));
    }
    let Some(Value::Object(spec)) = spec else {
        return Err(fail("it takes a table of `timeout` and `run`"));
    };
    if spec.get("run") != Some(&Value::Bool(true)) {
        return Err(fail("`run` must be a function"));
    }
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => return Err(fail("missing `timeout`")),
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0",
                ));
            }
        },
    };
    Ok(ValidSearch {
        name: name.clone(),
        timeout,
    })
}

/// Validates one `fiber.provider` call's `args`: exactly the checks
/// `fiber.provider` runs, in its order, over JSON. The provider name
/// stands where Lua names the provider file, which a process has none
/// of. `manifest_providers` are the manifest's declared providers, and
/// `registered` names the providers standing; both read as the registry
/// holds them, so an undeclared or duplicate registration fails exactly
/// where it would inline.
pub(crate) fn check_provider(
    args: &[Value],
    owner: &str,
    manifest_providers: &[String],
    registered: &BTreeSet<String>,
) -> Checked<ValidProvider> {
    let name = arg(args, 0);
    let spec = arg(args, 1);
    let violate = |message: String| Checked::Refuse(message);
    let (Some(Value::String(name)), Some(Value::Object(spec))) = (name, spec) else {
        return violate("fiber.provider: takes a name and a table of functions".to_owned());
    };
    for key in spec.keys() {
        if !matches!(
            key.as_str(),
            "models" | "quota" | "credential" | "sign" | "cost"
        ) {
            return violate(format!(
                "fiber.provider: `{key}` is not models, quota, credential, sign or cost"
            ));
        }
    }
    if !manifest_providers.iter().any(|declared| declared == name) {
        let error = Error::UndeclaredProvider {
            extension: owner.to_owned(),
            provider: name.clone(),
        };
        let message = error.to_string();
        let code = error.code();
        return Checked::Fail(code, message);
    }
    if registered.contains(name) {
        return violate(format!(
            "fiber.provider: `{name}` registers its provider exactly once"
        ));
    }
    let mut timeouts = BTreeMap::new();
    let mut funcs = Vec::new();
    for (key, func) in spec {
        let what = format!("fiber.provider: `{key}`");
        let Some(Value::Object(func)) = Some(func) else {
            return violate(format!(
                "{what}: `timeout` must be a whole number of milliseconds above 0"
            ));
        };
        let timeout = match func.get("timeout") {
            Some(value) => match millis(Some(value)) {
                Some(millis) => Duration::from_millis(millis),
                None => {
                    return violate(format!(
                        "{what}: `timeout` must be a whole number of milliseconds above 0"
                    ));
                }
            },
            None => {
                return violate(format!(
                    "{what}: `timeout` must be a whole number of milliseconds above 0"
                ));
            }
        };
        if func.get("run") != Some(&Value::Bool(true)) {
            return violate(format!("{what}: `run` must be a function"));
        }
        timeouts.insert(key.clone(), timeout);
        funcs.push(key.clone());
    }
    Checked::Accept(ValidProvider {
        name: name.clone(),
        timeouts,
        funcs,
    })
}

/// Validates one `fiber.harness` call's `args`: the documented
/// registration (`docs/extensions.md`, "Registering"), with the Lua
/// call's naming and timeout rules. Data keys ride along for the
/// delegate runner; only the functions run. The registry refuses a
/// duplicate name after this and mints the references.
pub(crate) fn check_harness(args: &[Value]) -> Result<ValidHarness, String> {
    let name = arg(args, 0);
    let spec = arg(args, 1);
    let fail = |why: &str| format!("`{}` harness not registered: {why}", shown(name));
    let Some(Value::String(name)) = name else {
        return Err(fail("the name must be a string"));
    };
    if !is_plain(name) {
        return Err(fail(&format!(
            "names are plain: `{name}` is empty or holds whitespace, `/` or `:`"
        )));
    }
    let Some(Value::Object(spec)) = spec else {
        return Err(fail(
            "it takes a table of `timeout`, `command`, `line`, `models` and optionally `quota`",
        ));
    };
    let timeout = match spec.get("timeout") {
        None | Some(Value::Null) => return Err(fail("missing `timeout`")),
        Some(value) => match millis(Some(value)) {
            Some(millis) => Duration::from_millis(millis),
            None => {
                return Err(fail(
                    "`timeout` must be a whole number of milliseconds above 0",
                ));
            }
        },
    };
    for key in ["command", "line", "models"] {
        if spec.get(key) != Some(&Value::Bool(true)) {
            return Err(fail(&format!("`{key}` must be a function")));
        }
    }
    let has_quota = match spec.get("quota") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(true)) => true,
        _ => return Err(fail("`quota` must be a function")),
    };
    let mut data = BTreeMap::new();
    for (key, value) in spec {
        if !matches!(
            key.as_str(),
            "timeout" | "command" | "line" | "models" | "quota"
        ) {
            data.insert(key.clone(), value.clone());
        }
    }
    let mut funcs = vec!["command".to_owned(), "line".to_owned(), "models".to_owned()];
    if has_quota {
        funcs.push("quota".to_owned());
    }
    Ok(ValidHarness {
        name: name.clone(),
        timeout,
        data,
        funcs,
    })
}
