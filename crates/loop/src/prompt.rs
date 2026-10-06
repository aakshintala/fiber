//! The preamble texts (`docs/system-prompt.md`, "The texts"): the one
//! `section` splitter every prompt file goes through, `fill` for its
//! placeholders, and the system prompt assembly.

use std::path::PathBuf;
use std::sync::Arc;

/// One extension section's files for the opening message: the extension's
/// name, its files' absolute paths in send order, and its byte budget.
pub(crate) type ExtensionSection = (String, Vec<PathBuf>, Option<u64>);

/// What a preamble build reads, once (`docs/system-prompt.md`, "The
/// system prompt" and `docs/prompt-cache.md`, "The preamble").
/// [`PromptInputs::new`] returns every optional input absent.
#[derive(Clone)]
pub struct PromptInputs {
    /// `SYSTEM.md` text.
    pub system: Option<String>,
    /// `APPEND_SYSTEM.md` text.
    pub append: Option<String>,
    /// The model's addendum.
    // debt: no provider data carries an addendum yet; fixed by #510.
    pub addendum: Option<String>,
    /// `(extension name, prompt text)`, any order.
    // debt: no extension runtime loads packages in `main`; fixed by #510.
    pub extensions: Vec<(String, String)>,
    /// The model's context window, in tokens.
    pub context_window: Option<u64>,
    /// Fiber home, for the global `AGENTS.md` and `skills/`.
    pub home: PathBuf,
    /// The person's home; `~/.agents/skills` is under it.
    pub agents_home: Option<PathBuf>,
    /// Each loaded extension's name and package directory, for its
    /// `skills/` and `prompts/`.
    pub extension_dirs: Vec<(String, PathBuf)>,
    /// Each extension section's files for the opening message, in send
    /// order.
    pub extension_sections: Vec<(String, Vec<PathBuf>, Option<u64>)>,
    /// `skills.disabled`, every layer unioned.
    pub skills_disabled: Vec<String>,
    /// The shell, or `unknown` when `SHELL` is unset.
    pub shell: String,
    /// The session log's path.
    pub session_log: String,
    /// The clock the opening message's date is read from.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// The credential label every request uses, which `preamble_built`
    /// records; absent when the provider takes no credential.
    pub credential: Option<String>,
}

impl PromptInputs {
    /// Every optional input absent: `home` is Fiber home, `shell` the
    /// shell or `unknown`, `session_log` the session log's path and
    /// `clock` the clock the date is read from.
    pub fn new(
        home: PathBuf,
        shell: String,
        session_log: String,
        clock: Arc<dyn contract::clock::Clock>,
    ) -> Self {
        Self {
            system: None,
            append: None,
            addendum: None,
            extensions: Vec::new(),
            context_window: None,
            home,
            agents_home: None,
            extension_dirs: Vec::new(),
            extension_sections: Vec::new(),
            skills_disabled: Vec::new(),
            shell,
            session_log,
            clock,
            credential: None,
        }
    }
}

const SYSTEM_MD: &str = include_str!("../prompt/system.md");
const MESSAGES_MD: &str = include_str!("../prompt/messages.md");

/// The section named `name` in `md`: from its `## name` line to the next
/// line starting `## `, with blank lines at either end removed.
pub(crate) fn section(md: &str, name: &str) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in md.lines() {
        if let Some(at) = line.strip_prefix("## ") {
            groups.push((at, vec![line]));
        } else if let Some((_, lines)) = groups.last_mut() {
            lines.push(line);
        }
    }
    let lines = groups
        .iter()
        .find(|(at, _)| *at == name)
        .map(|(_, lines)| lines.as_slice())
        .unwrap_or_default();
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |at| at + 1);
    lines
        .iter()
        .take(end)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// `template` with each `{name}` replaced by its value in `values`, left
/// to right, once: inserted text is never re-scanned, so a value holding
/// `{date}` stays as is. An unknown `{x}` is left alone.
pub(crate) fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(end) = after.find('}') else {
            out.push_str(after);
            return out;
        };
        let name = &after[1..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'/')
        {
            if let Some((_, value)) = values.iter().find(|(k, _)| *k == name) {
                out.push_str(value);
            } else {
                out.push_str(&after[..end + 1]);
            }
            rest = &after[end + 1..];
        } else {
            out.push('{');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The body of a `messages.md` section: its `## name` line dropped and
/// blank lines at either end removed, ready to `fill`.
pub(crate) fn body(md: &str, name: &str) -> String {
    section(md, name)
        .lines()
        .skip(1)
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn present(text: &str) -> Option<String> {
    let trimmed = text.trim_end().to_owned();
    if trimmed.trim().is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// The system prompt (`docs/system-prompt.md`, "The system prompt", R1
/// with R4, R5, P2 and P3). `tools` is `(name, guidelines, deferred)` in
/// any order; a deferred tool's guidelines are left out and the rest go
/// in tool-name order. Empty parts leave no blank line.
pub(crate) fn system_prompt(
    inputs: &PromptInputs,
    model: &str,
    unattended: bool,
    tools: &[(String, Option<String>, bool)],
) -> String {
    // Part 1: Fiber's text, or the person's SYSTEM.md in its place.
    let first = match inputs.system.as_deref().and_then(present) {
        Some(system) => system,
        None => SYSTEM_MD.trim_end().to_owned(),
    };
    // Part 2: the tools' guidelines in tool-name order.
    let tool_template = body(MESSAGES_MD, "tool");
    let tools_template = body(MESSAGES_MD, "tools");
    let mut ordered: Vec<&(String, Option<String>, bool)> = tools.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let mut filled_tools = Vec::new();
    for (name, guidelines, deferred) in ordered {
        if *deferred {
            continue;
        }
        let Some(text) = guidelines.as_deref().and_then(present) else {
            continue;
        };
        filled_tools.push(fill(
            &tool_template,
            &[("name", name), ("guidelines", &text)],
        ));
    }
    let second = if filled_tools.is_empty() {
        None
    } else {
        Some(fill(
            &tools_template,
            &[("guidelines", &filled_tools.join("\n\n"))],
        ))
    };
    // Part 3: the session section.
    let session_template = body(MESSAGES_MD, "session");
    let mut third = fill(&session_template, &[("model", model)]);
    if unattended {
        let line = body(MESSAGES_MD, "unattended");
        if !line.is_empty() {
            third.push_str("\n\n");
            third.push_str(&line);
        }
    }
    if let Some(addendum) = inputs.addendum.as_deref().and_then(present) {
        third.push_str("\n\n");
        third.push_str(&addendum);
    }
    // Part 4: each extension's text in extension-name order.
    let extension_template = body(MESSAGES_MD, "extension");
    let mut ordered_ext: Vec<&(String, String)> = inputs.extensions.iter().collect();
    ordered_ext.sort_by(|a, b| a.0.cmp(&b.0));
    let mut filled_ext = Vec::new();
    for (name, text) in ordered_ext {
        let Some(text) = present(text) else {
            continue;
        };
        filled_ext.push(fill(
            &extension_template,
            &[("extension", name), ("text", &text)],
        ));
    }
    let fourth = if filled_ext.is_empty() {
        None
    } else {
        Some(filled_ext.join("\n\n"))
    };
    // Part 5: the person's APPEND_SYSTEM.md.
    let fifth = inputs.append.as_deref().and_then(present);
    let mut parts = vec![first, third];
    if let Some(second) = second {
        parts.insert(1, second);
    }
    if let Some(fourth) = fourth {
        parts.push(fourth);
    }
    if let Some(fifth) = fifth {
        parts.push(fifth);
    }
    // Every part above is already trailing-trimmed; an empty one never
    // entered. Join with a blank line.
    parts.join("\n\n")
}

/// One preamble build: the system prompt text, the tool definitions in
/// name order for requests, and the `preamble_built` payload
/// (`docs/prompt-cache.md`, "The preamble" and `docs/events.md`,
/// "`preamble_built`"). `effort` and `thinking` are
/// absent: no thinking levels exist; `credential` is the inputs' label. `trigger_at` is
/// the automatic handoff's trigger, absent when it is off.
#[allow(
    clippy::too_many_arguments,
    reason = "the one build takes each preamble input; a struct would only rename them"
)]
pub(crate) fn build(
    inputs: &PromptInputs,
    model: &str,
    unattended: bool,
    tools: &std::collections::BTreeMap<String, crate::calls::Registered>,
    provider: &dyn contract::provider::Provider,
    reason: contract::events::PreambleReason,
    replaced: Vec<contract::events::ToolReplaced>,
    trigger_at: Option<u64>,
) -> (
    String,
    Vec<contract::provider::ToolDefinition>,
    contract::events::PreambleBuilt,
) {
    let infos: Vec<(String, Option<String>, bool)> = tools
        .iter()
        .map(|(name, (_, tool, definition))| (name.clone(), tool.guidelines(), definition.deferred))
        .collect();
    let system = system_prompt(inputs, model, unattended, &infos);
    let definitions: Vec<contract::provider::ToolDefinition> = tools
        .values()
        .map(|(_, _, definition)| definition.clone())
        .collect();
    let wire = provider.wire_tools(&definitions);
    let sent: Vec<contract::events::SentTool> = tools
        .iter()
        .zip(wire)
        .map(
            |((name, (by, _, definition)), definition_wire)| contract::events::SentTool {
                name: name.clone(),
                registered_by: by.clone(),
                deferred: definition.deferred,
                definition: definition_wire,
            },
        )
        .collect();
    let tool_choice = "auto".to_owned();
    let cache_lifetime = contract::events::CacheLifetime::OneHour;
    let event = contract::events::PreambleBuilt {
        reason,
        model: model.to_owned(),
        context_window: inputs.context_window.unwrap_or(0),
        trigger_at,
        effort: None,
        thinking: None,
        tool_choice,
        cache_lifetime,
        credential: inputs.credential.clone(),
        system_prompt: system.clone(),
        tools: sent,
        replaced,
    };
    (system, definitions, event)
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
