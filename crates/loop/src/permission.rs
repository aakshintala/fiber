//! Steps 1 to 6 of the permission order (`docs/permissions.md`, "The order a
//! call is judged in"): the pure judgment over one call, and the pure check
//! of a person's answer. No I/O and no log writes; `calls.rs` reads the rules
//! once per call, writes the decision lines and waits for a reply.

use std::path::Path;

use contract::commands::{RememberScope, ReplyAnswer};
use contract::events::{DecidedBy, Decision, Grant, RuleOffer, RuleScope, StandingRule};
use contract::rules::{RuleDecision, RulesError, StandingRules};
use contract::shapes::DeclaredEffects;
use contract::tool::Effects;

/// What steps 1 to 6 say about one call.
pub(crate) enum Verdict {
    /// Refused. `reason` is `tool_call_completed`'s reason; `why` is
    /// `permission_resolved`'s reason and the model's text.
    Deny {
        by: DecidedBy,
        reason: &'static str,
        why: String,
    },
    /// A standing ask matched: ask a person, naming the rule that asked.
    Ask(StandingRule),
    /// Allowed. `None` is a fast path, which writes no `permission_resolved`;
    /// `Some` says what the decision line reports.
    Allow(Option<DecidedBy>),
    /// Steps 1 to 6 said nothing: the reviewer decides (#294).
    Review,
}

/// Judges one call of `tool` with `effects`: the credential deny, a standing
/// deny, a standing ask, a fast path, a session grant, then a standing
/// allow. `rules` is both files read now; `grants` are the session's grants
/// so far. `credentials` is the resolved credentials directory.
pub(crate) fn judge(
    tool: &str,
    effects: &Effects,
    rules: &Result<StandingRules, RulesError>,
    grants: &[Grant],
    workspace: &Path,
    credentials: &Path,
) -> Verdict {
    if let Some(why) = credential_why(&effects.declared, workspace, credentials) {
        return Verdict::Deny {
            by: DecidedBy::CredentialDeny,
            reason: "credentials",
            why,
        };
    }
    let rules = match rules {
        Ok(rules) => rules,
        Err(error) => {
            return Verdict::Deny {
                by: DecidedBy::StandingRule,
                reason: "rules_unreadable",
                why: error.0.clone(),
            };
        }
    };
    let subject = effects.subject.as_deref();
    if find(rules, tool, subject, RuleDecision::Deny).is_some() {
        return Verdict::Deny {
            by: DecidedBy::StandingRule,
            reason: "standing_rule",
            why: "A standing rule refuses this call.".to_owned(),
        };
    }
    if let Some(rule) = find(rules, tool, subject, RuleDecision::Ask) {
        return Verdict::Ask(rule);
    }
    if super::calls::fast_path(&effects.declared, workspace) {
        return Verdict::Allow(None);
    }
    if grants
        .iter()
        .any(|grant| grant.tool == tool && matches(&grant.prefix, subject))
    {
        return Verdict::Allow(Some(DecidedBy::SessionGrant));
    }
    if find(rules, tool, subject, RuleDecision::Allow).is_some() {
        return Verdict::Allow(Some(DecidedBy::StandingRule));
    }
    Verdict::Review
}

/// Why the credential deny refuses a call with `declared` effects, if it
/// does: one of its paths touches the credentials directory. A path that
/// does not resolve counts as touching (fail closed). No paths, or no
/// declared paths, never matches.
pub(crate) fn credential_why(
    declared: &DeclaredEffects,
    workspace: &Path,
    credentials: &Path,
) -> Option<String> {
    let paths = declared.paths.as_ref()?;
    paths
        .iter()
        .any(|path| {
            // `join` replaces the workspace when the path is absolute.
            match super::calls::resolve(&workspace.join(path)) {
                None => true,
                Some(resolved) => resolved == credentials || resolved.starts_with(credentials),
            }
        })
        .then(|| "The call touches Fiber's credential directory.".to_owned())
}

/// The matching rule with `decision` for `tool`, the project's where both
/// files match. A deny or an ask included: a call no rule can safely match
/// matches nothing.
fn find(
    rules: &StandingRules,
    tool: &str,
    subject: Option<&str>,
    decision: RuleDecision,
) -> Option<StandingRule> {
    for (scope, list) in [
        (RuleScope::Project, &rules.project),
        (RuleScope::Global, &rules.global),
    ] {
        if let Some(rule) = list.iter().find(|rule| {
            rule.decision == decision && rule.tool == tool && matches(&rule.prefix, subject)
        }) {
            return Some(StandingRule {
                scope,
                prefix: rule.prefix.clone(),
            });
        }
    }
    None
}

/// Whether `prefix` matches `subject` (`docs/permissions.md`, "What a rule
/// matches"): a prefix ending in `/` matches every subject that starts with
/// it; any other prefix matches a subject equal to it, and one that goes on
/// with a space. A call with no subject matches nothing.
pub(crate) fn matches(prefix: &str, subject: Option<&str>) -> bool {
    let Some(subject) = subject else {
        return false;
    };
    if prefix.ends_with('/') {
        return subject.starts_with(prefix);
    }
    subject == prefix
        || subject
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(' '))
}

/// What a person's answer carries.
pub(crate) struct Answer {
    /// Allow or deny.
    pub decision: Decision,
    /// With a denial, what the person typed.
    pub feedback: Option<String>,
    /// With an allow, what is remembered and where.
    pub remember: Option<Remembered>,
}

/// What an allow remembers.
pub(crate) struct Remembered {
    /// A session grant, or a standing rule in the project's rules file.
    pub scope: RememberScope,
    /// The request's `rule.subject` or `rule.prefix`.
    pub prefix: String,
}

impl Remembered {
    /// The grant an allow of `tool`'s call remembers: the session grant its
    /// line carries, and the standing rule its line names.
    pub(crate) fn grant(&self, tool: &str) -> Grant {
        Grant {
            tool: tool.into(),
            prefix: self.prefix.clone(),
        }
    }
}

/// Checks `reply` against `offer`, the request's rule offer (`None` on a
/// standing ask, which offers nothing to remember). `None`: the reply does
/// not fit and is dropped.
pub(crate) fn answer(offer: Option<&RuleOffer>, reply: &ReplyAnswer) -> Option<Answer> {
    let ReplyAnswer::Approval {
        decision,
        feedback,
        remember,
    } = reply
    else {
        return None;
    };
    match decision {
        Decision::Deny => {
            if remember.is_some() {
                return None;
            }
            Some(Answer {
                decision: *decision,
                feedback: feedback.clone(),
                remember: None,
            })
        }
        Decision::Allow => {
            if feedback.is_some() {
                return None;
            }
            let remember = match (remember, offer) {
                (None, _) => None,
                (Some(remembered), Some(offer))
                    if remembered.prefix == offer.subject || remembered.prefix == offer.prefix =>
                {
                    Some(Remembered {
                        scope: remembered.scope,
                        prefix: remembered.prefix.clone(),
                    })
                }
                _ => return None,
            };
            Some(Answer {
                decision: *decision,
                feedback: None,
                remember,
            })
        }
    }
}

#[cfg(test)]
#[path = "permission_tests.rs"]
mod tests;
