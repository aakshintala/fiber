//! Per-tool result caps from configuration (`docs/tools.md`, "Bounded
//! results"; `docs/configuration.md`, "Keys"): each configured
//! `tools."<name>".max_result_bytes`, by the tool's registered name.

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::tool::{Ask, Bound, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value};

/// Each configured `tools."<name>".max_result_bytes`, by the tool's
/// registered name (`docs/configuration.md`, "Keys").
pub type ResultCaps = BTreeMap<String, u64>;

/// Applies `caps` to `tools`: each tool whose registered name has a
/// configured cap is asked for its own cut first (`Tool::with_cap`), and
/// otherwise wrapped so the loop's cut keeps that many bytes. A cap on a
/// tool that keeps both ends keeps the tool's declared proportions: `end =
/// floor(cap × declared.end ÷ (declared.start + declared.end))`, `start =
/// cap − end`, so the odd remainder goes to the head. Returns as many
/// entries as it was given, in the same order, each with its first string
/// unchanged. The lookup key is the tool's registered name,
/// `tool.definition().name`, never the first string.
pub fn capped(
    tools: Vec<(String, Arc<dyn Tool>)>,
    caps: &ResultCaps,
) -> Vec<(String, Arc<dyn Tool>)> {
    tools
        .into_iter()
        .map(|(by, tool)| {
            let name = tool.definition().name.clone();
            let Some(cap) = caps.get(&name) else {
                return (by, tool);
            };
            let cap = usize::try_from(*cap).unwrap_or(usize::MAX);
            match tool.with_cap(cap) {
                Some(own) => (by, own),
                None => {
                    let bound = split(tool.bound(), cap);
                    (by, Arc::new(Capped { inner: tool, bound }) as Arc<dyn Tool>)
                }
            }
        })
        .collect()
}

/// A tool the loop cuts to a configured cap: every method but `bound` is
/// the wrapped tool's.
struct Capped {
    inner: Arc<dyn Tool>,
    bound: Bound,
}

impl Tool for Capped {
    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        self.inner.effects(arguments)
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        self.inner.run(arguments, cancel, emit)
    }

    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        self.inner.run_asking(arguments, cancel, emit, ask)
    }

    fn bound(&self) -> Bound {
        self.bound
    }

    fn guidelines(&self) -> Option<String> {
        self.inner.guidelines()
    }
}

/// The loop's cut for a tool whose declared bound is `declared` under a
/// configured cap of `cap` bytes: `end = floor(cap × declared.end ÷
/// (declared.start + declared.end))`, `start = cap − end`, so the odd
/// remainder goes to the head. A declared bound of `{start: 0, end: 0}`
/// gets `{start: cap, end: 0}`. The arithmetic runs in `u128` with a
/// checked conversion back, so it never panics, wraps or casts.
fn split(declared: Bound, cap: usize) -> Bound {
    let wide = |n: usize| u128::try_from(n).unwrap_or(u128::MAX);
    let total = wide(declared.start) + wide(declared.end);
    if total == 0 {
        return Bound { start: cap, end: 0 };
    }
    let end = wide(cap) * wide(declared.end) / total;
    let end = usize::try_from(end).unwrap_or(usize::MAX);
    Bound {
        start: cap.saturating_sub(end),
        end,
    }
}

#[cfg(test)]
#[path = "caps_tests.rs"]
mod tests;
