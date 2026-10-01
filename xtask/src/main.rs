//! Fiber's CI and gate helpers, run as `cargo xtask <command>` from the
//! repository root. It implements the selection and checks in
//! `docs/ci.md`; `scripts/check` and the workflows call it. It is
//! development tooling: no Fiber crate depends on it and it never ships.
//!
//! Commands:
//!
//! - `select --base REV`: what a diff from the merge base with REV runs,
//!   uncommitted and untracked files included, as `key=value` lines;
//!   `packages` and `libraries` are bare names (for tools that only match
//!   workspace members, such as `cargo fmt -p`), `package_specs` and
//!   `library_specs` are `name@version` (for `cargo -p`, which resolves a
//!   bare name against the whole dependency graph and can be ambiguous)
//! - `plan --mode M --packages "A B" --event E --bug true|false`:
//!   which CI jobs run, as `key=value` lines
//! - `verdict`: reads `NEEDS` and `JOBS` from the environment and passes only
//!   if every selected job passed and every other job was skipped
//! - `ticket`: the issue the pull request body on stdin resolves
//! - `bug-filter FILE...`: the nextest filter, packages and test files among
//!   FILE, with how each is declared, as tab-separated lines
//! - `line-cap`, `unsafe-table`, `compiled-in`, `dependency-list`, `check-docs`: the checks

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool whose output is its interface"
)]

mod docs;
mod rules;
mod select;
#[cfg(test)]
mod test_dir;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde_json::Value;

use crate::rules::RustFile;
use crate::select::{Member, Members};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

/// Runs one command. Ok(false) means a check failed.
fn run(args: &[String]) -> Result<bool, String> {
    let (command, rest) = args
        .split_first()
        .ok_or("usage: cargo xtask <command>; see xtask/src/main.rs")?;
    match command.as_str() {
        "select" => {
            let members = workspace_members()?;
            let selection = select::classify(&changed_files(&flag(rest, "--base")?)?, &members);
            let libraries: Vec<&str> = selection
                .packages()
                .iter()
                .filter(|p| members.get(*p).is_some_and(|m| m.library))
                .map(String::as_str)
                .collect();
            let package_specs: Vec<String> = selection
                .packages()
                .iter()
                .map(|p| select::spec(p, &members))
                .collect();
            let library_specs: Vec<String> = libraries
                .iter()
                .map(|p| select::spec(p, &members))
                .collect();
            println!("mode={}", selection.mode());
            println!("packages={}", selection.packages().join(" "));
            println!("package_specs={}", package_specs.join(" "));
            println!("libraries={}", libraries.join(" "));
            println!("library_specs={}", library_specs.join(" "));
            Ok(true)
        }
        "plan" => {
            let packages: Vec<String> = flag(rest, "--packages")?
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let bug = flag(rest, "--bug")? == "true";
            let plan = select::plan(
                &flag(rest, "--mode")?,
                &packages,
                &flag(rest, "--event")?,
                bug,
            );
            let shards: Vec<u64> = (0..plan.shards).collect();
            println!(
                "jobs={}",
                Value::from(serde_json::Map::from_iter(
                    plan.jobs
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
                ))
            );
            println!("shards={}", Value::from(shards));
            println!("shard_total={}", plan.shards);
            Ok(true)
        }
        "verdict" => {
            let jobs: BTreeMap<String, bool> = env_object("JOBS")?
                .into_iter()
                .map(|(k, v)| (k, v.as_bool() == Some(true)))
                .collect();
            let results: BTreeMap<String, String> = env_object("NEEDS")?
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        v.get("result")
                            .and_then(Value::as_str)
                            .unwrap_or("missing")
                            .to_owned(),
                    )
                })
                .collect();
            let failures = select::verdict(&results, &jobs);
            report(
                "verdict",
                &failures,
                "every selected job passed and every other job was skipped",
            )
        }
        "ticket" => {
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("stdin: {e}"))?;
            if let Some(number) = select::ticket(&body) {
                println!("{number}");
            }
            Ok(true)
        }
        "bug-filter" => {
            let (filter, packages, files) = select::test_filter(rest, &workspace_members()?);
            println!("filter\t{filter}");
            for package in packages {
                println!("package\t{package}");
            }
            for file in files {
                let candidates: String = file
                    .declared_in
                    .iter()
                    .map(|(parent, declaration)| {
                        format!("\t{parent}\t{}", declaration.replace('\n', " "))
                    })
                    .collect();
                println!("file\t{}{candidates}", file.path);
            }
            Ok(true)
        }
        "line-cap" => report(
            "line-cap",
            &rules::over_cap(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "unsafe-table" => {
            let files = rust_files(&workspace_members()?)?;
            report(
                "unsafe-table",
                &rules::unsafe_mismatches(&files, &read("docs/code-quality.md")?)?,
                "ok",
            )
        }
        "compiled-in" => {
            let members = workspace_members()?;
            report(
                "compiled-in",
                &select::compiled_in_mismatches(&rust_files(&members)?, &members)?,
                "ok",
            )
        }
        "dependency-list" => {
            let failures = rules::unlisted(&cargo_dependencies()?, &read("docs/dependencies.md")?)?;
            report("dependency-list", &failures, "ok")
        }
        "check-docs" => {
            let root = Path::new(".");
            let mut failures = Vec::new();
            for path in docs::checked_files(root)? {
                failures.extend(docs::check_file(root, &path)?);
            }
            report("check-docs", &failures, "ok")
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn report(name: &str, failures: &[String], ok: &str) -> Result<bool, String> {
    for failure in failures {
        println!("{name}: {failure}");
    }
    if failures.is_empty() {
        println!("{name}: {ok}");
    }
    Ok(failures.is_empty())
}

fn flag(args: &[String], name: &str) -> Result<String, String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .ok_or(format!("{name} is required"))
}

/// The JSON object in environment variable `name`; empty when unset.
fn env_object(name: &str) -> Result<serde_json::Map<String, Value>, String> {
    let raw = std::env::var(name).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    if let Value::Object(map) = serde_json::from_str(&raw).map_err(|e| format!("{name}: {e}"))? {
        Ok(map)
    } else {
        Err(format!("{name} is not a JSON object"))
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{program}: {e}"))
}

fn git_lines(args: &[&str]) -> Result<Vec<String>, String> {
    Ok(output("git", args)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Files that differ from the merge base with `base`, uncommitted and
/// untracked changes included.
fn changed_files(base: &str) -> Result<Vec<String>, String> {
    let merge_base = git_lines(&["merge-base", base, "HEAD"])?
        .into_iter()
        .next()
        .ok_or("no merge base")?;
    let mut files: BTreeSet<String> = git_lines(&["diff", "--name-only", &merge_base])?
        .into_iter()
        .collect();
    files.extend(git_lines(&["ls-files", "--others", "--exclude-standard"])?);
    Ok(files.into_iter().collect())
}

fn packages() -> Result<(String, Vec<Value>), String> {
    let meta: Value = serde_json::from_str(&output(
        "cargo",
        &["metadata", "--format-version", "1", "--no-deps"],
    )?)
    .map_err(|e| format!("cargo metadata: {e}"))?;
    let root = meta
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or("cargo metadata: no workspace_root")?;
    let packages = meta
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata: no packages")?;
    Ok((root.to_owned(), packages.clone()))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A path under `root`, relative to it, with `/` separators.
fn relative(path: &Path, root: &str) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn workspace_members() -> Result<Members, String> {
    let (root, packages) = packages()?;
    let dir_of = |package: &Value| {
        relative(
            Path::new(text(package, "manifest_path"))
                .parent()
                .unwrap_or(Path::new("")),
            &root,
        )
    };
    let by_dir: BTreeMap<String, &str> = packages
        .iter()
        .map(|p| (dir_of(p), text(p, "name")))
        .collect();
    Ok(packages
        .iter()
        .map(|package| {
            let deps = array(package, "dependencies")
                .iter()
                .filter_map(|d| by_dir.get(&relative(Path::new(d.get("path")?.as_str()?), &root)))
                .map(|name| (*name).to_owned())
                .collect();
            let library = array(package, "targets")
                .iter()
                .any(|t| array(t, "kind").iter().any(|k| k.as_str() == Some("lib")));
            let member = Member {
                dir: dir_of(package),
                version: text(package, "version").to_owned(),
                deps,
                library,
            };
            (text(package, "name").to_owned(), member)
        })
        .collect())
}

/// (crate, dependency) for every dependency a workspace Cargo.toml names,
/// other than workspace members.
fn cargo_dependencies() -> Result<BTreeSet<(String, String)>, String> {
    let (_, packages) = packages()?;
    let names: BTreeSet<&str> = packages.iter().map(|p| text(p, "name")).collect();
    Ok(packages
        .iter()
        .flat_map(|p| {
            array(p, "dependencies")
                .iter()
                .map(move |d| (text(p, "name"), text(d, "name")))
        })
        .filter(|(_, dep)| !names.contains(dep))
        .map(|(krate, dep)| (krate.to_owned(), dep.to_owned()))
        .collect())
}

/// Every Rust file in a member, tracked or not, skipping ignored ones.
fn rust_files(members: &Members) -> Result<Vec<RustFile>, String> {
    let mut args = vec![
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend(members.values().map(|m| m.dir.as_str()));
    let mut files = Vec::new();
    for path in git_lines(&args)?.into_iter().collect::<BTreeSet<_>>() {
        let Some(krate) = select::owner(&path, members) else {
            continue;
        };
        let Some(member) = members.get(krate) else {
            continue;
        };
        if !path.ends_with(".rs") || !Path::new(&path).exists() {
            continue;
        }
        let rel = path
            .get(member.dir.len() + 1..)
            .unwrap_or_default()
            .to_owned();
        let source = read(&path)?;
        files.push(RustFile {
            krate: krate.to_owned(),
            path,
            rel,
            source,
        });
    }
    Ok(files)
}
