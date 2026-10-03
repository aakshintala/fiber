//! Minimal CLI for timing clap_complete's dynamic completion.
//! `sub remove <TAB>` completes the entry names in the directory named by
//! BENCH_HOME (stand-in for reading Fiber home).
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{ArgValueCompleter, CompleteEnv, CompletionCandidate};
use std::ffi::OsStr;

fn entries(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let Some(home) = std::env::var_os("BENCH_HOME") else {
        return Vec::new();
    };
    let Ok(dir) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(prefix.as_ref()))
        .collect();
    names.sort();
    names.into_iter().map(CompletionCandidate::new).collect()
}

#[derive(Parser)]
#[command(name = "bin", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Remove an installed extension.
    Remove {
        #[arg(add = ArgValueCompleter::new(entries))]
        name: String,
    },
    /// List extensions.
    List,
}

fn main() {
    CompleteEnv::with_factory(Cli::command).complete();
    let _ = Cli::parse();
}
