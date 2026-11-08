//! `fiber reviewer evidence --json` (`docs/invocation.md`, "Commands and
//! flags"): prints each topic's evidence as the panel gathers it, as one
//! JSON object keyed by topic in the setup's topic order, with credentials
//! removed from URLs as today. Raw `gather` proposes tally URLs verbatim
//! for the loop to redact, and this command has no loop, so it sanitizes
//! every URL-bearing tally field at the output boundary instead.

use std::io::{self, Write};
use std::sync::Arc;

use contract::notes::{Evidence, Topic, strip_userinfo};
use contract::{ErrorCode, SessionId};
use serde_json::{Value, json};

use crate::startup::Process;

/// Prints the topic-keyed evidence object and answers the process exit
/// code, following the local-command convention: the JSON object goes to
/// stdout, failures go to stderr with a nonzero exit.
pub(crate) fn run(json: bool, process: &Process) -> i32 {
    if !json {
        return crate::fail(crate::failed(
            ErrorCode::InvalidArguments,
            "reviewer evidence prints JSON: pass --json",
        ));
    }
    let home = match crate::startup::startup_home() {
        Ok(home) => home,
        Err(error) => return crate::fail(error),
    };
    let workspace = match crate::startup::startup_workspace() {
        Ok(workspace) => workspace,
        Err(error) => return crate::fail(error),
    };
    let key = match ::cli::project_of(&home, &workspace) {
        Ok((_, key)) => key,
        Err(error) => return crate::fail(error.into()),
    };
    let shell = tools::Shell::new(workspace.clone(), Arc::clone(&process.clock));
    let dir = log::session_dir(&home, key.as_str(), &SessionId("cli".to_owned()));
    let mut pairs = Vec::with_capacity(Topic::ALL.len());
    for topic in Topic::ALL {
        let items = doors::gather(&home, &dir, &workspace, topic, Some(&shell));
        pairs.push((
            topic_key(topic),
            Value::Array(items.iter().map(sanitized).collect()),
        ));
    }
    // The object keeps the setup's topic order, which the default keyed
    // map would sort away; names and values each serialize alone, so the
    // join adds only the braces, quotes, commas and colons.
    let mut out = String::from("{");
    for (index, (name, values)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(name);
        out.push_str("\":");
        out.push_str(&values.to_string());
    }
    out.push('}');
    // A closed stdout leaves nobody to tell, as `fiber --version` does.
    writeln!(io::stdout().lock(), "{out}").unwrap_or(());
    0
}

/// The topic's key: its serde name, which the evidence test pins against
/// the `Topic` serialization.
fn topic_key(topic: Topic) -> &'static str {
    match topic {
        Topic::SourceControl => "source_control",
        Topic::InternalHosts => "internal_hosts",
        Topic::InternalTools => "internal_tools",
        Topic::RoutineActions => "routine_actions",
        Topic::Never => "never",
        Topic::ReadOnlyCommands => "read_only_commands",
    }
}

/// One evidence item as the command prints it: the kind-tagged wire form
/// with credentials removed from the tally prefix, subject and text the
/// wire carries verbatim. Files items are already sanitised where gathered
/// and answers are the person's own words, so both print as gathered.
fn sanitized(item: &Evidence) -> Value {
    match item {
        Evidence::Files { text } => json!({"kind": "files", "text": text}),
        Evidence::Answer { text } => json!({"kind": "answer", "text": text}),
        Evidence::Session(tally) => json!({
            "kind": "session",
            "prefix": strip_userinfo(tally.prefix()),
            "count": tally.count(),
            "here": tally.here(),
            "text": strip_userinfo(tally.text()),
        }),
        Evidence::Allows(tally) => json!({
            "kind": "allows",
            "subject": strip_userinfo(tally.subject()),
            "count": tally.count(),
            "here": tally.here(),
            "text": strip_userinfo(tally.text()),
        }),
    }
}
