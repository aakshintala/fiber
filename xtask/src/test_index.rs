//! Compiler-reported test binaries and their source files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::select::{Members, TestRef, owner, spec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBinary {
    pub(crate) tests: Vec<TestRef>,
}

#[derive(Debug, Default)]
pub(crate) struct TestIndex {
    by_source: BTreeMap<String, Vec<TestBinary>>,
}

impl TestIndex {
    /// Build the test index for test files among `files` from nextest and
    /// the dep-info files nextest's binaries produce.
    pub(crate) fn build(
        root: &Path,
        files: &[String],
        members: &Members,
        profile: Option<&str>,
    ) -> Result<Self, String> {
        let packages = packages_for(files, members);
        if packages.is_empty() {
            return Ok(Self::default());
        }

        let mut args = vec![
            "nextest".to_owned(),
            "list".to_owned(),
            "--message-format".to_owned(),
            "json".to_owned(),
            "--verbose".to_owned(),
            "--color".to_owned(),
            "never".to_owned(),
            "--no-pager".to_owned(),
        ];
        if let Some(profile) = profile {
            args.push("--profile".to_owned());
            args.push(profile.to_owned());
        }
        for package in packages {
            args.push("-p".to_owned());
            args.push(spec(&package, members));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let json = crate::output("cargo", &refs)?;
        Self::from_list(root, &json)
    }

    /// Read nextest's listed binaries and each binary's compiler dep-info.
    pub(crate) fn from_list(root: &Path, json: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(json).map_err(|error| format!("nextest list: {error}"))?;
        let suites = value
            .get("rust-suites")
            .and_then(Value::as_object)
            .ok_or("nextest list: no rust-suites")?;
        let mut index = Self::default();
        for suite in suites.values() {
            let package = text_field(suite, "package-name");
            let binary_id = text_field(suite, "binary-id");
            let kind = text_field(suite, "kind");
            let binary_path = text_field(suite, "binary-path");
            if package.is_empty()
                || binary_id.is_empty()
                || kind.is_empty()
                || binary_path.is_empty()
            {
                return Err(
                    "nextest list: suite is missing package-name, binary-id, kind or binary-path"
                        .to_owned(),
                );
            }
            let dep_info = PathBuf::from(binary_path).with_extension("d");
            let dep_info_text = fs::read_to_string(&dep_info)
                .map_err(|error| format!("{}: {error}", dep_info.display()))?;
            let sources = dep_info_sources(root, &dep_info, &dep_info_text)?;
            let tests = listed_tests(suite, package, binary_id, kind);
            let binary = TestBinary { tests };
            for source in sources {
                let binaries = index.by_source.entry(source).or_default();
                if !binaries.contains(&binary) {
                    binaries.push(binary.clone());
                }
            }
        }
        Ok(index)
    }

    pub(crate) fn binaries_for(&self, source: &str) -> &[TestBinary] {
        self.by_source
            .get(source)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn from_fixtures(entries: Vec<(String, TestBinary)>) -> Self {
        let mut index = Self::default();
        for (source, binary) in entries {
            index.by_source.entry(source).or_default().push(binary);
        }
        index
    }
}

fn packages_for(files: &[String], members: &Members) -> Vec<String> {
    let mut packages = BTreeSet::new();
    for path in files {
        let Some(package) = owner(path, members) else {
            continue;
        };
        let Some(member) = members.get(package) else {
            continue;
        };
        if !member.unrun_tests.contains(path) {
            packages.insert(package.to_owned());
        }
    }
    packages.into_iter().collect()
}

fn listed_tests(suite: &Value, package: &str, binary_id: &str, kind: &str) -> Vec<TestRef> {
    let empty = serde_json::Map::new();
    let cases = suite
        .get("testcases")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let unit = kind == "lib" || kind == "bin";
    let mut tests = Vec::new();
    for (name, case) in cases {
        let status = case
            .get("filter-match")
            .and_then(|matched| matched.get("status"))
            .and_then(Value::as_str);
        if status != Some("matches") {
            continue;
        }
        let test = TestRef {
            package: package.to_owned(),
            binary: (!unit).then(|| binary_id.to_owned()),
            name: name.clone(),
            unit,
        };
        if !tests.contains(&test) {
            tests.push(test);
        }
    }
    tests
}

fn text_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn dep_info_sources(root: &Path, dep_info: &Path, text: &str) -> Result<Vec<String>, String> {
    let rule = text
        .lines()
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (_, dependencies) = rule
        .split_once(':')
        .ok_or_else(|| format!("{}: malformed dep-info", dep_info.display()))?;
    let mut sources = BTreeSet::new();
    for dependency in make_words(dependencies) {
        if !dependency.ends_with(".rs") {
            continue;
        }
        if let Some(path) = workspace_relative(root, Path::new(&dependency)) {
            sources.insert(path);
        }
    }
    Ok(sources.into_iter().collect())
}

/// Split the escaped dependency words in Cargo's first makefile rule.
fn make_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if escaped {
        word.push('\\');
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn workspace_relative(root: &Path, path: &Path) -> Option<String> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(root)
    };
    // Dep-info and current_dir can reach the same workspace through different symlink paths.
    let root = fs::canonicalize(&root).unwrap_or_else(|_| normalize(&root));
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let absolute = fs::canonicalize(&absolute).unwrap_or_else(|_| normalize(&absolute));
    absolute
        .strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
#[path = "test_index_tests.rs"]
mod tests;
