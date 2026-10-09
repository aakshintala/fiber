//! The `/usage` view: model-call totals grouped by turn, model and delegate
//! (`docs/tui.md`, "Swapped views").

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use contract::events::{DelegateStarted, UsageRecorded};
use contract::shapes::Usage;
use contract::{GenerationId, JobId, SessionId, TurnId};

use crate::format::money;
use crate::swapped::{Frame, List, Spot, about};

/// The latest call line per generation, and its first turn.
#[derive(Debug, Default)]
pub(crate) struct UsageFold {
    calls: BTreeMap<GenerationId, (Option<TurnId>, UsageRecorded)>,
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
        match self.calls.entry(line.generation_id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert((turn, line));
            }
            Entry::Occupied(mut entry) => entry.get_mut().1 = line,
        }
    }
}

/// The usage rows and footer for the swapped view.
pub(crate) fn frame(fold: &UsageFold, budget: Option<f64>, list: List) -> Frame {
    let mut rows = Vec::new();
    let calls: Vec<&UsageRecorded> = fold.calls.values().map(|(_, line)| line).collect();
    if calls.is_empty() {
        budget_row(&mut rows, budget, 0.0);
        rows.push(row("No model calls yet."));
    } else {
        let session = log::usage(calls.iter().copied());
        append_entry(&mut rows, "session", &session);
        budget_row(&mut rows, budget, session.cost.unwrap_or(0.0));

        rows.push(row("by turn"));
        let known_turns: BTreeSet<TurnId> = fold.turns.iter().cloned().collect();
        let mut turns: BTreeMap<TurnId, Vec<&UsageRecorded>> = BTreeMap::new();
        let mut outside = Vec::new();
        for (turn, line) in fold.calls.values() {
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

        rows.push(row("by model"));
        let mut models: BTreeMap<&str, Vec<&UsageRecorded>> = BTreeMap::new();
        for line in &calls {
            models.entry(&line.model).or_default().push(line);
        }
        for (model, group) in models {
            let usage = log::usage(group.iter().copied());
            append_entry(&mut rows, model, &usage);
        }

        rows.push(row("by delegate"));
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
    }
    Frame {
        title: "Usage".to_owned(),
        rows,
        list,
        below: Vec::new(),
        field: None,
        footer: "↑↓ scroll · Esc close".to_owned(),
    }
}

fn append_entry(rows: &mut Vec<Vec<(String, Option<Spot>)>>, heading: &str, usage: &Usage) {
    let tokens = &usage.tokens;
    let cache_write = tokens
        .cache_write
        .values()
        .fold(0u64, |sum, count| sum.saturating_add(*count));
    rows.push(row(heading));
    rows.push(row(format!(
        "tokens  in {} · cache read {} · cache write {} · out {}",
        about(tokens.input),
        about(tokens.cache_read),
        about(cache_write),
        about(tokens.output)
    )));
    let billed = usage.cost.map_or_else(|| "unknown".to_owned(), money);
    rows.push(row(format!(
        "cost  billed {billed} · on subscription {}",
        money(usage.subscription_cost)
    )));
}

fn budget_row(rows: &mut Vec<Vec<(String, Option<Spot>)>>, budget: Option<f64>, billed: f64) {
    if let Some(budget) = budget {
        let left = (budget - billed).max(0.0);
        rows.push(row(format!(
            "budget left  {} of {}",
            money(left),
            money(budget)
        )));
    }
}

fn row(text: impl Into<String>) -> Vec<(String, Option<Spot>)> {
    vec![(text.into(), None)]
}

#[cfg(test)]
#[path = "usage_view_tests.rs"]
mod tests;
