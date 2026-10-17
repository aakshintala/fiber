//! Known network hosts, folded only from trusted durable sources
//! (`docs/tools.md`, "Web fetch and web search").

use std::collections::BTreeSet;

use contract::events::{DecidedBy, Decision, Event, InputItem};
use contract::shapes::{ContentPart, Origin};

type Host = (String, u16);

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KnownHosts {
    hosts: BTreeSet<Host>,
}

impl KnownHosts {
    pub(crate) fn fold(&mut self, event: &Event) {
        if let Event::TurnStarted(turn) = event {
            for item in &turn.input {
                if let InputItem::Message {
                    content, sender, ..
                } = item
                {
                    if sender.origin == Origin::Driver {
                        self.message(content);
                    }
                }
            }
        }
        if let Event::SteeringApplied(steering) = event {
            if steering.sender.origin == Origin::Driver {
                self.message(&steering.content);
            }
        }
        if let Event::OpeningMessage(opening) = event {
            for file in &opening.instruction_files {
                self.hosts.extend(named_in(&file.content));
            }
        }
        if let Event::InstructionFile(file) = event {
            if file.extension.is_none() {
                if let Some(content) = &file.content {
                    self.hosts.extend(named_in(content));
                }
            }
        }
        if let Event::ToolCallCompleted(result) = event {
            self.hosts
                .extend(result.hosts_surfaced.iter().filter_map(|url| host_of(url)));
        }
        if let Event::PermissionResolved(resolved) = event {
            if resolved.decision == Decision::Allow
                && matches!(resolved.decided_by, DecidedBy::Person | DecidedBy::Reviewer)
            {
                if let Some(hosts) = &resolved.hosts {
                    self.hosts
                        .extend(hosts.iter().filter_map(|url| host_of(url)));
                }
            }
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
        hosts
            .iter()
            .all(|url| host_of(url).is_some_and(|host| self.hosts.contains(&host)))
    }
}

#[cfg(test)]
#[path = "known_hosts_tests.rs"]
mod tests;
