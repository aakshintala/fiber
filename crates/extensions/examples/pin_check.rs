//! The `pin_check` jig (`docs/testing.md`, "Jigs"): what checking a
//! repository's declared paths against `pinned.json` costs, in one process.
//! It builds a repository declaring one MCP server whose arguments name
//! `<files>` files of `<bytes>` bytes each, then times declaring and hashing
//! them cold (no `pinned.json`) and warm (every size and modification time
//! unchanged), and prints one line.
//!
//! `cargo run -p extensions --example pin_check -- [<files> [<bytes>]]`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "a jig measures real time; it never ships"
)]

use std::fs;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::json;

/// How many warm passes are timed; the line reports the fastest and the
/// slowest.
const WARM_PASSES: usize = 20;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let parse = |arg: Option<String>, default: usize| match arg {
        None => Some(default),
        Some(text) => text.parse().ok(),
    };
    let (Some(files), Some(bytes)) = (parse(args.next(), 200), parse(args.next(), 4096)) else {
        eprintln!("usage: cargo run -p extensions --example pin_check -- [<files> [<bytes>]]");
        return ExitCode::from(2);
    };
    match measure(files, bytes) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("pin_check: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn measure(files: usize, bytes: usize) -> Result<String, String> {
    let tmp = fakes::TempDir::new("fiber-pin-check");
    let (repo, home) = (tmp.path().join("repo"), tmp.path().join("home"));
    fs::create_dir_all(repo.join(".fiber")).map_err(|e| e.to_string())?;
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let names: Vec<String> = (0..files).map(|n| format!("data/f{n}.txt")).collect();
    fs::create_dir_all(repo.join("data")).map_err(|e| e.to_string())?;
    let content = "x".repeat(bytes);
    for name in &names {
        fs::write(repo.join(name), &content).map_err(|e| e.to_string())?;
    }
    let config = json!({"mcp": {"servers": {"db": {"command": "node", "args": names}}}});
    fs::write(repo.join(".fiber/config.json"), config.to_string()).map_err(|e| e.to_string())?;

    let clock = fakes::clock::SystemClock;
    let pass = || -> Result<(Duration, String), String> {
        let start = Instant::now();
        let items = extensions::declared_items(&repo, &clock).map_err(|e| e.to_string())?;
        let mut index = extensions::Index::load(&home);
        let item = items.first().ok_or("no item declared")?;
        let hash = extensions::hash(&mut index, item).map_err(|e| e.to_string())?;
        index.save().map_err(|e| e.to_string())?;
        Ok((start.elapsed(), hash))
    };
    let (cold, hash) = pass()?;
    let mut warm = Vec::new();
    for _ in 0..WARM_PASSES {
        let (took, again) = pass()?;
        if again != hash {
            return Err("a warm pass gave another hash".into());
        }
        warm.push(took);
    }
    let (fastest, slowest) = (warm.iter().min(), warm.iter().max());
    let ms = |d: Option<&Duration>| d.map_or(0.0, |d| d.as_secs_f64() * 1000.0);
    Ok(format!(
        "{files} files of {bytes} bytes ({} bytes) on {}-{}: cold {:.2} ms; warm {:.2} ms fastest, {:.2} ms slowest of {WARM_PASSES}",
        files * bytes,
        std::env::consts::OS,
        std::env::consts::ARCH,
        cold.as_secs_f64() * 1000.0,
        ms(fastest),
        ms(slowest),
    ))
}

