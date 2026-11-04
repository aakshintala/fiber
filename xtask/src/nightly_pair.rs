//! The `nightly-pair` subcommand (`docs/ci.md`, "Releases"): the shared
//! nightly-tag predicate and the previous-nightly lookup the nightly update
//! check drives. A version is bare, a tag carries `v`: the predicate matches
//! release tags only, and every mode normalizes once through
//! [`tag_for_version`], following `crates/extensions/src/update.rs`.

use std::io::Read;

/// The nightly tag for a bare version: the single conversion from version
/// to tag form, so no caller adds `v` in wiring.
pub(crate) fn tag_for_version(version: &str) -> String {
    format!("v{version}")
}

/// Whether `tag` is a nightly release tag: `v` plus a dotted triple plus
/// `-nightly.` plus an eight-digit date, and nothing else. Bare versions
/// are not tags, so they never satisfy the predicate.
pub(crate) fn is_nightly_tag(tag: &str) -> bool {
    nightly_key(tag).is_some()
}

/// The numeric order of a nightly tag: the base triple, then the date.
/// Anything that is not a strict nightly release tag has no key.
fn nightly_key(tag: &str) -> Option<([u64; 3], u64)> {
    let rest = tag.strip_prefix('v')?;
    let (base, date) = rest.split_once("-nightly.")?;
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = date.parse().ok()?;
    let mut parts = base.split('.');
    let mut numbers = [0; 3];
    for slot in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((numbers, date))
}

/// The greatest nightly tag in `tags` that is not `exclude_tag`, compared
/// numerically by base triple then date. Non-nightly entries never win;
/// the exclusion matches on the tag form exactly.
pub(crate) fn prev_excluding(tags: &[&str], exclude_tag: &str) -> Option<String> {
    tags.iter()
        .filter_map(|tag| nightly_key(tag).map(|key| (key, *tag)))
        .filter(|(_, tag)| *tag != exclude_tag)
        .max()
        .map(|(_, tag)| tag.to_owned())
}

/// The `--prev-excluding` decision over a supplied tag listing: the
/// previous nightly for a bare version, erroring naming the exclusion when
/// nothing else is listed.
pub(crate) fn prev_excluding_command(exclude_bare: &str, input: &str) -> Result<String, String> {
    let tags: Vec<&str> = input.lines().collect();
    let excluded = tag_for_version(exclude_bare);
    prev_excluding(&tags, &excluded).ok_or_else(|| {
        format!(
            "nightly-pair: no nightly tag besides {excluded}; the previous-nightly lookup found nothing to install"
        )
    })
}

/// Runs the subcommand: `--is-nightly <bare-version>` exits 0 exactly when
/// the tag form matches, and `--prev-excluding <bare-version>` prints the
/// previous nightly over the tag list on stdin, erroring naming the
/// exclusion when nothing else is listed.
pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let [mode, value] = args else {
        return Err(usage());
    };
    if mode == "--is-nightly" {
        return Ok(is_nightly_tag(&tag_for_version(value)));
    }
    if mode == "--prev-excluding" {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("stdin: {error}"))?;
        let tag = prev_excluding_command(value, &input)?;
        println!("{tag}");
        return Ok(true);
    }
    Err(usage())
}

/// The subcommand's usage line, shared by every arity and flag fault.
fn usage() -> String {
    "usage: cargo xtask nightly-pair (--is-nightly|--prev-excluding) <bare-version>".to_owned()
}

#[cfg(test)]
#[path = "nightly_pair_tests.rs"]
mod tests;
