//! Compiler-index filters for changed test files (`bug-filter`).

use std::collections::BTreeSet;

use super::{Members, owner};
use crate::rules::RustFile;
use crate::test_index::TestIndex;

/// Whether `rel`, a path relative to its crate, is a test file for the
/// source-size and timeout rules.
pub(crate) fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs" || name.ends_with("_tests.rs") || rel.starts_with("tests/")
}

/// The test files among `files` that a plain nextest run never builds:
/// those of a `[[test]]` target declaring `test = false`.
pub(crate) fn test_files(
    files: &[String],
    members: &Members,
    sources: &[RustFile],
) -> Result<Vec<String>, String> {
    let mut test_files = Vec::new();
    for path in files {
        let Some(package) = owner(path, members) else {
            continue;
        };
        let Some(member) = members.get(package) else {
            continue;
        };
        let rel = path.get(member.dir.len() + 1..).unwrap_or_default();
        if member.unrun_tests.iter().any(|unrun| unrun == path) {
            continue;
        }
        if rel.starts_with("tests/") {
            test_files.push(path.clone());
        } else if let Some(source) = sources.iter().find(|source| source.path == *path)
            && super::changed_tests::has_test_functions(&source.source)
                .map_err(|error| format!("{path}: {error}"))?
        {
            test_files.push(path.clone());
        }
    }
    Ok(test_files)
}

pub(crate) fn unrun_test_files(files: &[String], members: &Members) -> Vec<String> {
    files
        .iter()
        .filter(|path| {
            owner(path, members)
                .and_then(|name| members.get(name))
                .is_some_and(|member| member.unrun_tests.contains(path))
        })
        .cloned()
        .collect()
}

/// The nextest filter selecting the changed test files, using their
/// compiler-reported binaries and test names.
pub(crate) fn test_filter(
    files: &[String],
    members: &Members,
    sources: &[RustFile],
    index: &TestIndex,
) -> Result<(String, Vec<String>), String> {
    let mut tests = Vec::new();
    let mut packages = BTreeSet::new();
    for path in test_files(files, members, sources)? {
        if index.binaries_for(&path).is_empty() {
            // The runner did not compile this source file, as with a
            // platform-gated target. It contributes no tests on this runner.
            continue;
        }
        let selected = super::tests_in_files(std::slice::from_ref(&path), members, sources, index)?;
        if selected.is_empty() {
            return Err(format!("{path}: compiler index maps this file to no tests"));
        }
        for test in selected {
            packages.insert(test.package.clone());
            if !tests.contains(&test) {
                tests.push(test);
            }
        }
    }
    Ok((super::filter(&tests), packages.into_iter().collect()))
}
