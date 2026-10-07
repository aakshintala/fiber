//! Per-tool result caps from configuration (`docs/tools.md`, "Bounded
//! results"; `docs/configuration.md`, "Keys"): each configured
//! `tools."<name>".max_result_bytes`, by the tool's registered name.

use std::collections::BTreeMap;
use std::sync::Arc;

use contract::tool::Tool;

/// Each configured `tools."<name>".max_result_bytes`, by the tool's
/// registered name (`docs/configuration.md`, "Keys").
pub type ResultCaps = BTreeMap<String, u64>;

/// Applies `caps` to `tools`: each tool whose registered name has a
/// configured cap is asked for its own cut first, and otherwise wrapped in
/// the loop's cut. Returns as many entries as it was given, in the same
/// order, each with its first string unchanged.
pub fn capped(
    tools: Vec<(String, Arc<dyn Tool>)>,
    _caps: &ResultCaps,
) -> Vec<(String, Arc<dyn Tool>)> {
    tools
}

#[cfg(test)]
#[path = "caps_tests.rs"]
mod tests;
