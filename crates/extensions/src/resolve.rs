//! Which version of an extension to install (`docs/extensions.md`,
//! "Versions"): the lowest tag that meets every stated minimum.

use std::collections::BTreeMap;

use crate::Error;

/// `0.3.0` or `v0.3.0` as three numbers compared in order
/// (`docs/dependencies.md`, "Written ourselves").
pub(crate) fn version(text: &str) -> Result<[u64; 3], Error> {
    parse(text, 3)
}

/// A minimum, such as `1.2`, `v1.2` or `v1.2.0`: missing numbers are 0.
fn minimum(text: &str) -> Result<[u64; 3], Error> {
    parse(text, 1)
}

fn parse(text: &str, least: usize) -> Result<[u64; 3], Error> {
    let bad = || Error::BadVersion { text: text.into() };
    let mut out = [0; 3];
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut seen = 0;
    for slot in &mut out {
        match parts.next() {
            Some(p) => *slot = p.parse().map_err(|_| bad())?,
            None => break,
        }
        seen += 1;
    }
    if seen < least || parts.next().is_some() {
        return Err(bad());
    }
    Ok(out)
}

/// The tags that are versions, lowest first; equal versions by tag text.
fn versions(tags: &[String]) -> Vec<([u64; 3], &String)> {
    let mut found: Vec<_> = tags
        .iter()
        .filter_map(|t| version(t).ok().map(|v| (v, t)))
        .collect();
    found.sort();
    found
}

/// The newest tag that is a version, for an install or update that names no
/// minimum.
pub(crate) fn newest(tags: &[String]) -> Option<String> {
    versions(tags).last().map(|(_, t)| (*t).clone())
}

/// The highest minimum in `wants` (requirer to minimum). Minimums of two
/// major versions stop with both named.
fn floor(name: &str, wants: &BTreeMap<String, String>) -> Result<[u64; 3], Error> {
    let mut floor = [0; 3];
    let mut first: Option<(&String, &String, u64)> = None;
    for (requirer, min) in wants {
        let v = minimum(min)?;
        match first {
            Some((a, a_min, major)) if major != v[0] => {
                return Err(Error::MajorConflict {
                    name: name.into(),
                    a: format!("`{a}` needs {a_min}"),
                    b: format!("`{requirer}` needs {min}"),
                });
            }
            Some(_) => {}
            None => first = Some((requirer, min, v[0])),
        }
        floor = floor.max(v);
    }
    Ok(floor)
}

/// Whether the version `have` meets every minimum in `wants`.
pub(crate) fn meets(
    name: &str,
    have: &str,
    wants: &BTreeMap<String, String>,
) -> Result<bool, Error> {
    let floor = floor(name, wants)?;
    Ok(version(have).is_ok_and(|v| v >= floor && v[0] == floor[0]))
}

/// The lowest tag at or above every minimum in `wants`; the newest tag is
/// not taken unless a minimum asks for it.
pub(crate) fn pick(
    name: &str,
    wants: &BTreeMap<String, String>,
    tags: &[String],
) -> Result<String, Error> {
    let floor = floor(name, wants)?;
    versions(tags)
        .into_iter()
        .find(|(v, _)| *v >= floor && v[0] == floor[0])
        .map(|(_, t)| t.clone())
        .ok_or_else(|| Error::NoVersion {
            name: name.into(),
            needs: wants
                .values()
                .max_by_key(|m| minimum(m).ok())
                .cloned()
                .unwrap_or_default(),
        })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
