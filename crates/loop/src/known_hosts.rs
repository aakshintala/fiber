//! Known network hosts, folded only from trusted durable sources
//! (`docs/tools.md`, "Web fetch and web search").

use std::collections::{BTreeMap, BTreeSet};

use contract::events::{DecidedBy, Decision, Event, InputItem, InstructionReason};
use contract::shapes::{ContentPart, Origin};

type Host = (String, u16);

/// The URL's host and port, normalised to the scheme's default port when absent.
// debt: host_of mirrors crates/tools/src/web_fetch/target.rs host_and_port, ceiling: two parsers, upgrade trigger: a shared normaliser in contract, or a cross-crate test, once the fast path changes
fn host_of(url: &str) -> Option<Host> {
    let (scheme, rest) = url.split_once("://")?;
    let default = if scheme.eq_ignore_ascii_case("https") {
        443
    } else if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        let port = if tail.is_empty() {
            ""
        } else {
            tail.strip_prefix(':')?
        };
        (host, port)
    } else {
        authority.split_once(':').unwrap_or((authority, ""))
    };
    if host.is_empty() || !host.is_ascii() || host.contains(['%', '[', ']', ' ', '\t', '\n', '\r'])
    {
        return None;
    }
    let port = if port.is_empty() {
        default
    } else {
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        port.parse().ok()?
    };
    Some((host.to_ascii_lowercase(), port))
}

fn named_in(text: &str) -> impl Iterator<Item = Host> + '_ {
    text.char_indices().filter_map(move |(at, _)| {
        let tail = text.get(at..)?;
        if !["http://", "https://"].iter().any(|scheme| {
            tail.get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
        }) {
            return None;
        }
        let url = tail
            .split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')'))
            .next()?;
        host_of(url.trim_end_matches(['.', ',', ';', ':', '!', '?']))
    })
}

/// The session's known network hosts, folded only from trusted durable sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KnownHosts {
    hosts: BTreeSet<Host>,
    // Keep instruction provenance so a later session edit excludes every version.
    instruction_hosts: BTreeMap<String, BTreeSet<Host>>,
    changed_paths: BTreeSet<String>,
}

impl KnownHosts {
    pub(crate) fn fold(&mut self, event: &Event) {
        match event {
            Event::TurnStarted(turn) => {
                for item in &turn.input {
                    if let InputItem::Message {
                        content, sender, ..
                    } = item
                        && sender.origin == Origin::Driver
                    {
                        self.message(content);
                    }
                }
            }
            Event::SteeringApplied(steering) => {
                if steering.sender.origin == Origin::Driver {
                    self.message(&steering.content);
                }
            }
            Event::OpeningMessage(opening) => {
                for file in &opening.instruction_files {
                    if !self.changed_paths.contains(&file.path) {
                        self.instruction_hosts
                            .entry(file.path.clone())
                            .or_default()
                            .extend(named_in(&file.content));
                    }
                }
            }
            Event::InstructionFile(file) => {
                if file.extension.is_none() {
                    if file.reason == InstructionReason::OwnEdit {
                        self.changed_paths.insert(file.path.clone());
                        self.instruction_hosts.remove(&file.path);
                    }
                    if !self.changed_paths.contains(&file.path)
                        && let Some(content) = &file.content
                    {
                        self.instruction_hosts
                            .entry(file.path.clone())
                            .or_default()
                            .extend(named_in(content));
                    }
                }
            }
            Event::ToolCallCompleted(result) => {
                for change in result.changes.iter().flatten() {
                    self.changed_paths.insert(change.path.clone());
                    self.instruction_hosts.remove(&change.path);
                }
                self.hosts
                    .extend(result.hosts_surfaced.iter().filter_map(|url| host_of(url)));
            }
            Event::PermissionResolved(resolved) => {
                if resolved.decision == Decision::Allow
                    && matches!(resolved.decided_by, DecidedBy::Person | DecidedBy::Reviewer)
                    && let Some(hosts) = &resolved.hosts
                {
                    self.hosts
                        .extend(hosts.iter().filter_map(|url| host_of(url)));
                }
            }
            Event::FiberStarted(_) => {}
            Event::FiberExited(_) => {}
            Event::SessionStarted(_) => {}
            Event::Rewound(_) => {}
            Event::Moved(_) => {}
            Event::StepStarted(_) => {}
            Event::TurnCompleted(_) => {}
            Event::SteeringQueue(_) => {}
            Event::ShellCommand(_) => {}
            Event::SessionNamed(_) => {}
            Event::Clients(_) => {}
            Event::SessionStatus(_) => {}
            Event::ContextAdded(_) => {}
            Event::AssistantMessageStarted(_) => {}
            Event::AssistantMessageDelta(_) => {}
            Event::AssistantMessageCompleted(_) => {}
            Event::TextCompleted(_) => {}
            Event::ToolCallArgumentsDelta(_) => {}
            Event::ReasoningStarted(_) => {}
            Event::ReasoningDelta(_) => {}
            Event::ReasoningCompleted(_) => {}
            Event::ToolCallRequested(_) => {}
            Event::ToolCallStarted(_) => {}
            Event::ToolCallDelta(_) => {}
            Event::PermissionRequested(_) => {}
            Event::InteractionRequested(_) => {}
            Event::InteractionResolved(_) => {}
            Event::RepositoryCodeOffered(_) => {}
            Event::RepositoryCodeResolved(_) => {}
            Event::UsageRecorded(_) => {}
            Event::QuotaNoticed(_) | Event::QuotaRearmed(_) => {}
            Event::RetryScheduled(_) => {}
            Event::Notice(_) => {}
            Event::PreambleBuilt(_) => {}
            Event::ModelChanged(_) => {}
            Event::DateChanged(_) => {}
            Event::SkillsChanged(_) => {}
            Event::HandoffStarted(_) => {}
            Event::HandoffCompleted(_) => {}
            Event::SkillsResent(_) => {}
            Event::ContextNudged(_) => {}
            Event::ReviewerKept(_) => {}
            Event::McpServerFailed(_) => {}
            Event::McpServerReady(_) => {}
            Event::Reloaded(_) => {}
            Event::ExtensionsLoaded(_) => {}
            Event::ExtensionStateSet(_) => {}
            Event::ExtensionStateUnset(_) => {}
            Event::ExtensionUi(_) => {}
            Event::ExtensionMessage(_) => {}
            Event::ExtensionLog(_) => {}
            Event::ExtensionHello(_) => {}
            Event::ExtensionRequest(_) => {}
            Event::ExtensionExec(_) => {}
            Event::JobStarted(_) => {}
            Event::DelegateStarted(_) => {}
            Event::JobDelta(_) => {}
            Event::JobLine(_) => {}
            Event::DelegateFinished(_) => {}
            Event::JobCompleted(_) => {}
            Event::JobsPendingNotified(_) => {}
            Event::CommandAccepted(_) => {}
            Event::CommandRejected(_) => {}
        }
    }

    fn message(&mut self, content: &[ContentPart]) {
        for part in content {
            if let ContentPart::Text { text } = part {
                self.hosts.extend(named_in(text));
            }
        }
    }

    pub(crate) fn all_known(&self, hosts: &[String]) -> bool {
        hosts.iter().all(|url| {
            host_of(url).is_some_and(|host| {
                self.hosts.contains(&host)
                    || self
                        .instruction_hosts
                        .values()
                        .any(|hosts| hosts.contains(&host))
            })
        })
    }
}

#[cfg(test)]
#[path = "known_hosts_tests.rs"]
mod tests;
