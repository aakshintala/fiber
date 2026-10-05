//! The qualified name of one MCP tool (`docs/mcp.md`, "Tools and their
//! names"): `mcp__<server>__<tool>`, cut short with a hash suffix when a
//! protocol's length limit would refuse it.

/// The longest qualified name Fiber declares. A live probe of each vendor
/// sets the final value (`#588`'s probe hold); 64 is the placeholder every
/// protocol is known to accept.
pub(crate) const MAX_NAME_LEN: usize = 64;

/// Hex chars of SHA-256 carried by a cut name, so two long names sharing a
/// prefix stay distinct. Sized by the same probe as [`MAX_NAME_LEN`].
pub(crate) const HASH_LEN: usize = 8;

/// `mcp__<server>__<tool>`, or the cut form when that runs over
/// [`MAX_NAME_LEN`]: the first `MAX_NAME_LEN - 1 - HASH_LEN` chars of the
/// full name, `_`, then the first [`HASH_LEN`] lowercase hex chars of
/// SHA-256 over the full untruncated name. Cut by chars, never by bytes,
/// so the result is always valid UTF-8. A name at or under the limit is
/// unchanged.
pub(crate) fn qualified(server: &str, tool: &str) -> String {
    let full = format!("mcp__{server}__{tool}");
    if full.chars().count() <= MAX_NAME_LEN {
        return full;
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, full.as_bytes());
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let suffix = hex.get(..HASH_LEN).unwrap_or(&hex);
    let keep = MAX_NAME_LEN - 1 - HASH_LEN;
    let head: String = full.chars().take(keep).collect();
    format!("{head}_{suffix}")
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
