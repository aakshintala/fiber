//! Which version of an extension to install (`docs/extensions.md`,
//! "Versions"): the lowest tag that meets every stated minimum.

use std::collections::BTreeMap;

use crate::Error;

/// `0.3.0` or `v0.3.0` as three numbers compared in order
/// (`docs/dependencies.md`, "Written ourselves").
pub fn version(text: &str) -> Result<[u64; 3], Error> {
    parse(text, 3)
}

/// A nightly tag's base version and date: `v0.3.0-nightly.20240101` is
/// `([0, 3, 0], "20240101"`). The suffix is a literal `-nightly.` plus
/// exactly 8 ASCII digits; anything else is [`Error::BadVersion`], as is
/// a base [`version`] refuses.
pub fn nightly_parts(text: &str) -> Result<([u64; 3], String), Error> {
    let bad = || Error::BadVersion { text: text.into() };
    let (base, date) = text.rsplit_once("-nightly.").ok_or_else(bad)?;
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    Ok((version(base).map_err(|_| bad())?, date.to_owned()))
}

/// Whether `have` meets the `need` minimum, nightly-aware: a version
/// parses as a stable triple, a nightly tag as its base and date; a
/// nightly sorts below the stable release on its base, and two nightlies
/// on one base order by date. Anything else is [`Error::BadVersion`].
pub(crate) fn at_least(need: &str, have: &str) -> Result<bool, Error> {
    Ok(version_key(need)? <= version_key(have)?)
}

/// A release version as an orderable key: the base triple, whether the
/// release is a stable one (stable sorts above a nightly on the same
/// base), and the nightly date, which orders two nightlies on one base.
/// A nightly-form version keeps its date, anything else parses as a
/// stable triple.
pub fn version_key(text: &str) -> Result<([u64; 3], bool, String), Error> {
    match nightly_parts(text) {
        Ok((base, date)) => Ok((base, false, date)),
        Err(_) => version(text).map(|base| (base, true, String::new())),
    }
}

/// A minimum as an orderable key: the base triple, whether the minimum
/// is a stable one (a stable release sorts above a nightly on the same
/// base), and the nightly date, which orders two nightlies on one base.
/// A correctly shaped nightly form orders with its base triple; a short
/// stable form such as `1.2` fills missing numbers with 0. Anything
/// else is [`Error::BadVersion`].
fn minimum(text: &str) -> Result<([u64; 3], bool, String), Error> {
    if let Ok((base, date)) = nightly_parts(text) {
        return Ok((base, false, date));
    }
    parse(text, 1).map(|base| (base, true, String::new()))
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

/// The highest minimum in `wants` (requirer to minimum), as the same
/// orderable key [`minimum`] returns. Minimums of two major versions
/// stop with both named.
fn floor(name: &str, wants: &BTreeMap<String, String>) -> Result<([u64; 3], bool, String), Error> {
    let mut floor = ([0; 3], true, String::new());
    let mut first: Option<(&String, &String, u64)> = None;
    for (requirer, min) in wants {
        let v = minimum(min)?;
        match first {
            Some((a, a_min, major)) if major != v.0[0] => {
                return Err(Error::MajorConflict {
                    name: name.into(),
                    a: format!("`{a}` needs {a_min}"),
                    b: format!("`{requirer}` needs {min}"),
                });
            }
            Some(_) => {}
            None => first = Some((requirer, min, v.0[0])),
        }
        floor = floor.max(v);
    }
    Ok(floor)
}

/// Whether the version `have` meets every minimum in `wants`: a
/// correctly shaped nightly form orders below the stable release on
/// its base, exactly as the minima do. A `have` that is no version at
/// all stays a miss, so the update restages it.
pub(crate) fn meets(
    name: &str,
    have: &str,
    wants: &BTreeMap<String, String>,
) -> Result<bool, Error> {
    let floor = floor(name, wants)?;
    let Ok(have) = version_key(have) else {
        return Ok(false);
    };
    Ok(have >= floor && have.0[0] == floor.0[0])
}

/// Whether the version being installed, `have`, meets every minimum
/// installed extensions state for it: a different major version stops with
/// both named, and a lower one is an error too.
pub(crate) fn root_meets(
    name: &str,
    have: &str,
    wants: &BTreeMap<String, String>,
) -> Result<(), Error> {
    let mut all = wants.clone();
    all.insert("this request".into(), have.into());
    floor(name, &all)?;
    if meets(name, have, wants)? {
        return Ok(());
    }
    Err(Error::NoVersion {
        name: name.into(),
        needs: wants
            .values()
            .max_by_key(|m| minimum(m).ok())
            .cloned()
            .unwrap_or_default(),
    })
}

/// The newest tag within the major version every minimum in `wants`
/// accepts: the update-everything choice for one installed extension.
/// No constraints means the newest tag, as for an install that names no
/// minimum. A minimum no tag meets, or two major versions, fails as for
/// an ordinary dependency.
pub(crate) fn newest_within(
    name: &str,
    wants: &BTreeMap<String, String>,
    tags: &[String],
) -> Result<String, Error> {
    if wants.is_empty() {
        return newest(tags).ok_or_else(|| Error::NoTag { name: name.into() });
    }
    let floor = floor(name, wants)?;
    // Tags stay stable-only: each tag orders as the stable key it is,
    // against a floor that may be a nightly.
    versions(tags)
        .into_iter()
        .rev()
        .find(|(v, _)| (*v, true, String::new()) >= floor && v[0] == floor.0[0])
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

/// The lowest tag at or above every minimum in `wants`; the newest tag is
/// not taken unless a minimum asks for it.
pub(crate) fn pick(
    name: &str,
    wants: &BTreeMap<String, String>,
    tags: &[String],
) -> Result<String, Error> {
    let floor = floor(name, wants)?;
    // Tags stay stable-only: each tag orders as the stable key it is,
    // against a floor that may be a nightly.
    versions(tags)
        .into_iter()
        .find(|(v, _)| (*v, true, String::new()) >= floor && v[0] == floor.0[0])
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
