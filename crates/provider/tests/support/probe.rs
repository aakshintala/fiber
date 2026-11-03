//! What the `probe` jig shares with its binary test: the probe's logic
//! (argument parsing, request builders, the three-step run, verdicts,
//! report lines, exit code). The jig includes this file with `#[path]`,
//! and `tests/probe_jig.rs` does the same; the unit tests live in
//! `probe_tests.rs`, so they run inside that binary.

#![allow(dead_code, reason = "each user of this module needs only part of it")]

use std::sync::Arc;

use contract::ActionId;
use contract::events::CacheLifetime;
use contract::provider::{
    CallError, Input, ModelRequest, Provider, Reply, ReplyAction, TOOL_SEARCH, ToolDefinition,
};
use contract::shapes::Tokens;

/// The system prompt holds at least this many characters, over every
/// Anthropic cache minimum, so a cache that holds shows a read.
pub(crate) const SYSTEM_MIN_CHARS: usize = 20_000;

/// Every probe tool description holds at least this many characters.
pub(crate) const TOOL_MIN_CHARS: usize = 100;

/// The tool the load leaves ready to call.
const CREATE: &str = "probe_calendar_create";

/// The calendar tools the probe declares, by name.
const CALENDAR: [&str; 3] = [CREATE, "probe_calendar_list", "probe_calendar_delete"];

/// The person message on all three requests.
const USER_MESSAGE: &str = "Add a calendar event titled standup. The calendar tools are not declared yet: call tool_search with the query \"calendar\" to find them, then call the right one.";

/// One step of the probe's three requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The request with the tools in full.
    Baseline,
    /// The first request with the tools deferred.
    Deferred,
    /// The request after the load, replaying the first reply.
    AfterLoad,
}

/// How a request's failure reads: refused key, rejected deferral, or
/// something the run cannot tell deferral from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The key was refused.
    Auth,
    /// The provider rejected deferred loading.
    Fail,
    /// Anything else, including a dropped connection.
    Inconclusive,
}

/// Which class a failing request at `step` with this HTTP status falls in.
/// 401 and 403 are the refused key at every step; 400 and 422 reject
/// deferral on the deferred steps and read as the probe itself on the
/// baseline; every other status, a failure inside a 200 stream and a
/// failure with no status read as inconclusive.
pub(crate) fn classify(status: Option<u16>, step: Step) -> Kind {
    match status {
        Some(401) | Some(403) => Kind::Auth,
        Some(400) | Some(422) => match step {
            Step::Baseline => Kind::Inconclusive,
            Step::Deferred | Step::AfterLoad => Kind::Fail,
        },
        Some(_) | None => Kind::Inconclusive,
    }
}

/// One check's verdict. `Fail` carries its own detail; `Inconclusive`
/// prints as `fail (inconclusive: <detail>)`; `NotEstablished` prints as
/// `fail (not established: <detail>)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The check passed.
    Pass(String),
    /// The check failed.
    Fail(String),
    /// The check could not tell.
    Inconclusive(String),
    /// The run stopped before the check.
    NotEstablished(String),
}

/// The probe's outcome: either the key was refused, or both checks'
/// verdicts. The refusal lives in this one place, so the report and the
/// exit code cannot disagree about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The key was refused, with the refused status.
    KeyRefused(u16),
    /// Both checks' verdicts.
    Decided {
        /// Whether the model searched first and called the loaded tool after.
        load: Verdict,
        /// Whether the request after the load read the cache.
        cache: Verdict,
    },
}

/// Whether the request after the load read the cache: any read at all.
pub(crate) fn cache_held(cache_read: u64) -> bool {
    cache_read > 0
}

/// The input tokens the request after the load reports: uncached input
/// plus reads plus every write class.
pub(crate) fn total_input(tokens: &Tokens) -> u64 {
    tokens.input + tokens.cache_read + tokens.cache_write.values().sum::<u64>()
}

/// The jig's parsed arguments.
///
/// `Debug` redacts header values: a header may carry a credential
/// (`Authorization: Bearer …`), so only header names print.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// `anthropic-messages` or `openai-responses`.
    pub(crate) protocol: String,
    /// The provider's base URL.
    pub(crate) base_url: String,
    /// The model id.
    pub(crate) model: String,
    /// The key: a file path, or `env:NAME`.
    pub(crate) key_spec: String,
    /// Whether `--bearer` was passed.
    pub(crate) bearer: bool,
    /// The `--store=false` value, when passed.
    pub(crate) store: Option<bool>,
    /// The `NAME:VALUE` headers to send.
    pub(crate) headers: Vec<(String, String)>,
}

impl std::fmt::Debug for Parsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parsed")
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("key_spec", &self.key_spec)
            .field("bearer", &self.bearer)
            .field("store", &self.store)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| (name, "[redacted]"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Parses the jig's arguments: the four fixed words, then options in any
/// order. Anything else is rejected before any request is sent.
pub(crate) fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let [protocol, base_url, model, key_spec, rest @ ..] = args else {
        return Err("probe: expected PROTOCOL BASE_URL MODEL KEY [--bearer] [--store=false] [NAME:VALUE]...".into());
    };
    match protocol.as_str() {
        "anthropic-messages" | "openai-responses" => {}
        other => {
            return Err(format!(
                "probe: {other} is not a protocol; use anthropic-messages or openai-responses"
            ));
        }
    }
    let mut parsed = Parsed {
        protocol: protocol.clone(),
        base_url: base_url.clone(),
        model: model.clone(),
        key_spec: key_spec.clone(),
        bearer: false,
        store: None,
        headers: Vec::new(),
    };
    for arg in rest {
        if arg == "--bearer" {
            parsed.bearer = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--store=") {
            if value != "false" {
                return Err(format!("probe: --store takes only false, not {value}"));
            }
            parsed.store = Some(false);
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            return Err(format!("probe: unknown option --{flag}"));
        }
        let Some((name, value)) = arg.split_once(':') else {
            return Err(format!("probe: a header is NAME:VALUE, not {arg}"));
        };
        parsed
            .headers
            .push((name.to_owned(), value.trim().to_owned()));
    }
    Ok(parsed)
}

/// Reads the key named by `spec`: `env:NAME` through `env`, else a file.
/// The error names the variable or path, never the value.
pub(crate) fn key_from(spec: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let raw = match spec.strip_prefix("env:") {
        Some(name) => {
            env(name).ok_or_else(|| format!("probe: {name}: environment variable is not set"))?
        }
        None => std::fs::read_to_string(spec).map_err(|e| format!("probe: {spec}: {e}"))?,
    };
    let key = raw.trim().to_owned();
    if key.is_empty() {
        return Err("probe: the key is empty".into());
    }
    Ok(key)
}

/// The probe's system prompt: filler over every Anthropic cache minimum.
fn system_prompt() -> String {
    const PARAGRAPH: &str = "You are a calendar assistant under test. Calendar events carry titles and start times, and every change applies at once. The probe asks for one event titled standup, then checks that the calendar tools load only when the model searches for them. Nothing here sends mail or invites guests; the calendar stays exactly as the run found it. ";
    let mut prompt = String::from("You are a calendar assistant under test. ");
    while prompt.len() < SYSTEM_MIN_CHARS {
        prompt.push_str(PARAGRAPH);
    }
    prompt
}

/// The probe's `tool_search`: declared, never deferred.
fn search_tool() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SEARCH.into(),
        description: "Searches the deferred tools by name, description and parameter names, and loads the best matches so you can call them.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1}
            },
            "required": ["query"]
        }),
        deferred: false,
        hosted: None,
    }
}

/// One calendar tool: deferred in the deferred requests, full in the base.
fn calendar_tool(name: &str, description: &str, deferred: bool) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"]
        }),
        deferred,
        hosted: None,
    }
}

/// The four probe tools, the calendar three deferred as told.
fn tools(deferred: bool) -> Vec<ToolDefinition> {
    vec![
        search_tool(),
        calendar_tool(
            CALENDAR[0],
            "Creates one calendar event with the given title on the default calendar, starting at the next free hour for thirty minutes. The event stays private: no guest gets an invitation and no other calendar changes.",
            deferred,
        ),
        calendar_tool(
            CALENDAR[1],
            "Lists the events on the default calendar for the coming seven days, newest first, with each entry's title and start time. Entry notes and guest lists stay hidden; the answer carries titles and times only.",
            deferred,
        ),
        calendar_tool(
            CALENDAR[2],
            "Deletes the one calendar event with the exact title from the default calendar. The delete applies at once and keeps no undo entry, so titles must match exactly, including case, before calling.",
            deferred,
        ),
    ]
}

/// One probe request: the same system prompt, tool choice, caps and cache
/// key; the conversation holds only the person message.
fn base_request(reference: &str, deferred: bool) -> ModelRequest {
    ModelRequest {
        system_prompt: system_prompt(),
        tools: tools(deferred),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: format!("probe-{reference}"),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: Some(2000),
        conversation: Arc::from(vec![Input::User {
            text: USER_MESSAGE.into(),
            images: Vec::new(),
        }]),
        media: Default::default(),
    }
}

/// The baseline request: every tool in full.
pub(crate) fn baseline_request(reference: &str) -> ModelRequest {
    base_request(reference, false)
}

/// The first deferred request: the calendar tools deferred.
pub(crate) fn first_request(reference: &str) -> ModelRequest {
    base_request(reference, true)
}

/// The request after the load: the first request's prefix, plus the first
/// reply replayed and its search answered. `previous_end` marks where the
/// first request ended, so the prefix bytes match.
pub(crate) fn after_load_request(reference: &str, first: &Reply) -> ModelRequest {
    let mut request = first_request(reference);
    let mut conversation = vec![Input::User {
        text: USER_MESSAGE.into(),
        images: Vec::new(),
    }];
    let mut search: Option<ActionId> = None;
    for (index, action) in first.actions.iter().enumerate() {
        let id = ActionId(format!("probe-{}", index + 1));
        match action {
            ReplyAction::Text(part) => conversation.push(Input::Assistant {
                model: reference.into(),
                text: part.text.clone(),
                provider_item: part.provider_item.clone(),
            }),
            ReplyAction::Reasoning(part) => conversation.push(Input::Reasoning {
                model: reference.into(),
                text: part.text.clone(),
                provider_item: part.provider_item.clone(),
            }),
            ReplyAction::ToolCall(call) => {
                if call.name == TOOL_SEARCH && search.is_none() {
                    search = Some(id.clone());
                }
                conversation.push(Input::ToolCall {
                    action_id: id,
                    call: call.clone(),
                    model: reference.into(),
                });
            }
            ReplyAction::Hosted(_) | ReplyAction::HostedResult { .. } => {}
        }
    }
    if let Some(action_id) = search {
        conversation.push(Input::ToolSearchResult {
            action_id,
            loaded: vec![CREATE.into()],
        });
        for input in conversation.clone() {
            if let Input::ToolCall {
                action_id, call, ..
            } = &input
                && call.name != TOOL_SEARCH
            {
                conversation.push(Input::ToolResult {
                    action_id: action_id.clone(),
                    text: "not run by the probe".into(),
                    is_error: true,
                    images: Vec::new(),
                    pdfs: Vec::new(),
                });
            }
        }
    }
    request.conversation = Arc::from(conversation);
    request.previous_end = Some(1);
    request
}

/// Whether `name` is a calendar tool the probe declared.
fn is_calendar(name: &str) -> bool {
    CALENDAR.contains(&name)
}

/// Checks one reply's tool calls against the call rule with `loaded` tools
/// loaded. With `search_first`, the first tool call must find the tools;
/// everywhere, no call may name a calendar tool outside `loaded`, and a
/// reply with no tool call, an undeclared name or a hosted call cannot
/// tell.
fn check_calls(
    actions: &[ReplyAction],
    loaded: &[&str],
    search_first: bool,
) -> Result<(), Verdict> {
    if actions.iter().any(|action| {
        matches!(
            action,
            ReplyAction::Hosted(_) | ReplyAction::HostedResult { .. }
        )
    }) {
        return Err(Verdict::Inconclusive(
            "the reply called a hosted tool".into(),
        ));
    }
    let mut calls = actions.iter().filter_map(|action| match action {
        ReplyAction::ToolCall(call) => Some(call),
        ReplyAction::Text(_)
        | ReplyAction::Reasoning(_)
        | ReplyAction::Hosted(_)
        | ReplyAction::HostedResult { .. } => None,
    });
    if search_first {
        match calls.next() {
            None => {
                return Err(Verdict::Inconclusive("the model made no tool call".into()));
            }
            Some(first) if first.name == TOOL_SEARCH => {}
            Some(first) if is_calendar(&first.name) => {
                return Err(Verdict::Fail(format!(
                    "called {} before loading it",
                    first.name
                )));
            }
            Some(first) => {
                return Err(Verdict::Inconclusive(format!(
                    "the model called a tool the probe never declared: {}",
                    first.name
                )));
            }
        }
    }
    for call in calls {
        if call.name == TOOL_SEARCH {
            continue;
        }
        if is_calendar(&call.name) {
            if !loaded.contains(&call.name.as_str()) {
                return Err(Verdict::Fail(format!(
                    "called {} before loading it",
                    call.name
                )));
            }
        } else {
            return Err(Verdict::Inconclusive(format!(
                "the model called a tool the probe never declared: {}",
                call.name
            )));
        }
    }
    Ok(())
}

/// Sends one request, returning the reply or the failure's status with the
/// provider's own message, already redacted by the call.
fn send(provider: &dyn Provider, request: &ModelRequest) -> Result<Reply, (Option<u16>, String)> {
    match provider.call(request).run(&mut |_| {}) {
        Ok(reply) => Ok(reply),
        Err(CallError::Failed { failure, .. }) => {
            let status = failure.provider.as_ref().and_then(|found| found.status);
            let message = failure
                .provider
                .as_ref()
                .map_or_else(|| failure.message.clone(), |found| found.message.clone());
            Err((status, message))
        }
        Err(CallError::Cancelled { .. }) => Err((None, "the call was cancelled".into())),
    }
}

/// The outcome when `step`'s request fails with `status` and `message`.
fn failed(step: Step, status: Option<u16>, message: &str) -> Outcome {
    if let Some(code @ (401 | 403)) = status {
        return Outcome::KeyRefused(code);
    }
    let detail = match status {
        Some(code) => format!("HTTP {code}: {message}"),
        None => message.to_owned(),
    };
    let (load, cache) = match step {
        Step::Baseline => {
            let baseline = match status {
                Some(code) => format!("baseline: HTTP {code}"),
                None => format!("baseline: {message}"),
            };
            (
                Verdict::Inconclusive(baseline),
                Verdict::NotEstablished("baseline inconclusive".into()),
            )
        }
        Step::Deferred | Step::AfterLoad => match classify(status, step) {
            Kind::Fail => (
                Verdict::Fail(detail),
                Verdict::NotEstablished("load rejected".into()),
            ),
            Kind::Inconclusive | Kind::Auth => (
                Verdict::Inconclusive(detail),
                Verdict::NotEstablished("load did not complete".into()),
            ),
        },
    };
    Outcome::Decided { load, cache }
}

/// Runs the probe: the baseline with the tools in full, the first request
/// with them deferred, then the request after the load. At most three
/// requests, one each, stopping at the first refusal, rejection or
/// inconclusive step.
pub(crate) fn run(provider: &dyn Provider, reference: &str) -> Outcome {
    if let Err((status, message)) = send(provider, &baseline_request(reference)) {
        return failed(Step::Baseline, status, &message);
    }
    let first = match send(provider, &first_request(reference)) {
        Ok(reply) => reply,
        Err((status, message)) => return failed(Step::Deferred, status, &message),
    };
    if let Err(verdict) = check_calls(&first.actions, &[], true) {
        return Outcome::Decided {
            load: verdict,
            cache: Verdict::NotEstablished("load did not complete".into()),
        };
    }
    let after = match send(provider, &after_load_request(reference, &first)) {
        Ok(reply) => reply,
        Err((status, message)) => return failed(Step::AfterLoad, status, &message),
    };
    let loaded = [CREATE];
    let load = match check_calls(&after.actions, &loaded, false) {
        Err(verdict) => verdict,
        Ok(()) => {
            let mut called = after.actions.iter().filter_map(|action| match action {
                ReplyAction::ToolCall(call) => Some(call),
                ReplyAction::Text(_)
                | ReplyAction::Reasoning(_)
                | ReplyAction::Hosted(_)
                | ReplyAction::HostedResult { .. } => None,
            });
            if called.any(|call| call.name == CREATE) {
                Verdict::Pass("searched, loaded probe_calendar_create, then called it".into())
            } else {
                Verdict::Inconclusive("the model did not call the loaded tool".into())
            }
        }
    };
    let read = after.tokens.cache_read;
    let total = total_input(&after.tokens);
    let detail = format!("cache read {read} of {total} input tokens");
    let cache = if cache_held(read) {
        Verdict::Pass(detail)
    } else {
        Verdict::Fail(detail)
    };
    Outcome::Decided { load, cache }
}

/// One check's line: `pass` or `fail` with its detail.
fn check_line(check: &str, verdict: &Verdict) -> String {
    match verdict {
        Verdict::Pass(detail) => format!("{check}: pass ({detail})"),
        Verdict::Fail(detail) => format!("{check}: fail ({detail})"),
        Verdict::Inconclusive(detail) => format!("{check}: fail (inconclusive: {detail})"),
        Verdict::NotEstablished(detail) => format!("{check}: fail (not established: {detail})"),
    }
}

/// The probe's report lines: the header, both checks, then the verdict.
pub(crate) fn report(protocol: &str, model: &str, outcome: &Outcome) -> Vec<String> {
    let mut lines = vec![format!("probe: {protocol} {model}")];
    match outcome {
        Outcome::KeyRefused(status) => {
            lines.push("load: fail (not established: key refused)".into());
            lines.push("cache: fail (not established: key refused)".into());
            lines.push(format!("probe: key refused (HTTP {status})"));
        }
        Outcome::Decided { load, cache } => {
            lines.push(check_line("load", load));
            lines.push(check_line("cache", cache));
            let inconclusive = match load {
                Verdict::Inconclusive(detail) => Some(detail),
                Verdict::Pass(_) | Verdict::Fail(_) | Verdict::NotEstablished(_) => match cache {
                    Verdict::Inconclusive(detail) => Some(detail),
                    Verdict::Pass(_) | Verdict::Fail(_) | Verdict::NotEstablished(_) => None,
                },
            };
            lines.push(match inconclusive {
                Some(detail) => format!("probe: cannot tell ({detail})"),
                None => match load {
                    Verdict::Fail(_) => "probe: fail".into(),
                    Verdict::Pass(_) | Verdict::Inconclusive(_) | Verdict::NotEstablished(_) => {
                        match cache {
                            Verdict::Fail(_) => "probe: fail".into(),
                            Verdict::Pass(_)
                            | Verdict::Inconclusive(_)
                            | Verdict::NotEstablished(_) => "probe: pass".into(),
                        }
                    }
                },
            });
        }
    }
    lines
}

/// The jig's exit code: 4 when the key was refused, 1 when a check failed,
/// 3 when it cannot tell, else 0.
pub(crate) fn exit_code(outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::KeyRefused(_) => 4,
        Outcome::Decided { load, cache } => {
            if matches!(load, Verdict::Fail(_)) || matches!(cache, Verdict::Fail(_)) {
                return 1;
            }
            if matches!(load, Verdict::Inconclusive(_)) || matches!(cache, Verdict::Inconclusive(_))
            {
                return 3;
            }
            0
        }
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
