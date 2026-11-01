//! The `/usage` view: model-call totals grouped by turn, model and delegate
//! (`docs/tui.md`, "Swapped views").

use std::collections::{BTreeMap, BTreeSet};

use contract::events::{DelegateStarted, UsageRecorded};
use contract::shapes::Usage;
use contract::{JobId, SessionId, TurnId};

use crate::format::{Spend, money};
use crate::swapped::{Frame, Ink, List, Spot, compact};

/// The latest call line per generation, and its first turn.
#[derive(Debug, Default)]
pub(crate) struct UsageFold {
    spend: Spend,
    turns: Vec<TurnId>,
    delegates: Vec<(SessionId, String)>,
    jobs: BTreeMap<JobId, String>,
}

impl UsageFold {
    /// Keeps a turn once, in arrival order.
    pub(crate) fn turn_started(&mut self, turn: TurnId) {
        if !self.turns.contains(&turn) {
            self.turns.push(turn);
        }
    }

    /// Keeps a job's description for the delegate that follows it.
    pub(crate) fn job_started(&mut self, job: JobId, description: String) {
        self.jobs.insert(job, description);
    }

    /// Labels a delegate with its job description and model.
    pub(crate) fn delegate_started(&mut self, started: &DelegateStarted) {
        if self
            .delegates
            .iter()
            .any(|(session, _)| session == &started.delegate_session_id)
        {
            return;
        }
        let description = self
            .jobs
            .get(&started.job_id)
            .cloned()
            .unwrap_or_else(|| started.job_id.0.clone());
        self.delegates.push((
            started.delegate_session_id.clone(),
            format!("◆ {description} · {}", started.model),
        ));
    }

    /// Keeps the first turn for a generation and its latest usage line.
    pub(crate) fn recorded(&mut self, turn: Option<TurnId>, line: UsageRecorded) {
        self.spend.record(turn, line);
    }
}

/// The usage rows and footer for the swapped view: one bold heading
/// per group, one row per entry, the budget dim and last
/// (`docs/tui.md`, "Swapped views").
pub(crate) fn frame(fold: &UsageFold, budget: Option<f64>, list: List) -> Frame {
    let mut rows = Vec::new();
    let calls: Vec<&UsageRecorded> = fold.spend.iter().map(|(_, line)| line).collect();
    if calls.is_empty() {
        rows.push(row("No model calls yet.", Ink::Muted));
        budget_row(&mut rows, budget, 0.0);
    } else {
        let session = log::usage(calls.iter().copied());
        rows.push(row("By turn", Ink::Heading));
        let known_turns: BTreeSet<TurnId> = fold.turns.iter().cloned().collect();
        let mut turns: BTreeMap<TurnId, Vec<&UsageRecorded>> = BTreeMap::new();
        let mut outside = Vec::new();
        for (turn, line) in fold.spend.iter() {
            match turn.as_ref().filter(|turn| known_turns.contains(*turn)) {
                Some(turn) => turns.entry(turn.clone()).or_default().push(line),
                None => outside.push(line),
            }
        }
        for (number, turn) in fold.turns.iter().enumerate() {
            let Some(group) = turns.get(turn) else {
                continue;
            };
            let title = format!("turn {}", number.saturating_add(1));
            let usage = log::usage(group.iter().copied());
            append_entry(&mut rows, &title, &usage);
        }
        if !outside.is_empty() {
            let usage = log::usage(outside.iter().copied());
            append_entry(&mut rows, "outside a turn", &usage);
        }

        rows.push(row("By model", Ink::Heading));
        let mut models: BTreeMap<&str, Vec<&UsageRecorded>> = BTreeMap::new();
        for line in &calls {
            models.entry(&line.model).or_default().push(line);
        }
        for (model, group) in models {
            let usage = log::usage(group.iter().copied());
            append_entry(&mut rows, model, &usage);
        }

        rows.push(row("By delegate", Ink::Heading));
        let mut delegates: BTreeMap<SessionId, Vec<&UsageRecorded>> = BTreeMap::new();
        let mut this_session = Vec::new();
        for line in &calls {
            match &line.origin_session_id {
                Some(origin) => delegates.entry(origin.clone()).or_default().push(*line),
                None => this_session.push(*line),
            }
        }
        if !this_session.is_empty() {
            let usage = log::usage(this_session.iter().copied());
            append_entry(&mut rows, "this session", &usage);
        }
        for (session, label) in &fold.delegates {
            if let Some(group) = delegates.remove(session) {
                let usage = log::usage(group.iter().copied());
                append_entry(&mut rows, label, &usage);
            }
        }
        for (session, group) in delegates {
            let label = format!("session {}", session.0);
            let usage = log::usage(group.iter().copied());
            append_entry(&mut rows, &label, &usage);
        }
        budget_row(&mut rows, budget, session.cost.unwrap_or(0.0));
    }
    Frame {
        joins: Vec::new(),
        title: "Usage".to_owned(),
        command: "/usage".to_owned(),
        rows,
        list,
        below: Vec::new(),
        field: None,
        footer: String::new(),
    }
}

/// One entry's row: its name, compact counts in and out, the cache
/// kinds only when non-zero, and what it billed (`docs/tui.md`,
/// "Swapped views").
fn append_entry(rows: &mut Vec<Vec<(String, Option<Spot>, Ink)>>, name: &str, usage: &Usage) {
    let tokens = &usage.tokens;
    let mut counts = format!(
        "{name} · {} in · {} out",
        compact(tokens.input),
        compact(tokens.output)
    );
    if tokens.cache_read > 0 {
        counts.push_str(&format!(" · {} cache read", compact(tokens.cache_read)));
    }
    let cache_write = tokens
        .cache_write
        .values()
        .fold(0u64, |sum, count| sum.saturating_add(*count));
    if cache_write > 0 {
        counts.push_str(&format!(
            " · {cache_write} cache write",
            cache_write = compact(cache_write)
        ));
    }
    let billed = usage.cost.map_or_else(|| "unknown".to_owned(), money);
    counts.push_str(&format!(
        " · {billed} billed · {} subscription",
        money(usage.subscription_cost)
    ));
    rows.push(row(counts, Ink::Plain));
}

fn budget_row(rows: &mut Vec<Vec<(String, Option<Spot>, Ink)>>, budget: Option<f64>, billed: f64) {
    if let Some(budget) = budget {
        let left = (budget - billed).max(0.0);
        rows.push(row(
            format!("budget left {} of {}", money(left), money(budget)),
            Ink::Muted,
        ));
    }
}

fn row(text: impl Into<String>, ink: Ink) -> Vec<(String, Option<Spot>, Ink)> {
    vec![(text.into(), None, ink)]
}

#[cfg(test)]
#[path = "usage_view_tests.rs"]
mod tests;
