//! The `provider-server` jig (`docs/testing.md`, "Jigs"): serves a scripted or
//! recorded stream on a local port and prints each request it receives.
//!
//! `cargo run -p fakes --example provider-server -- [STATUS:]FILE...`
//!
//! Each argument is one response, served in order: the file's bytes as the
//! body, as a 200 event stream, or as a JSON body with STATUS, such as
//! `429:rate-limited.json`. Stop it with Ctrl-C.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;
use std::time::Duration;

use fakes::{Deadline, ProviderServer};

#[path = "../tests/support/provider_server.rs"]
mod support;

use support::{response, show};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: cargo run -p fakes --example provider-server -- [STATUS:]FILE...");
        return ExitCode::from(2);
    }
    let mut script = Vec::new();
    for arg in &args {
        match response(arg) {
            Ok(r) => script.push(r),
            Err(e) => {
                eprintln!("provider-server: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let server = match ProviderServer::start(script) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provider-server: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("listening on {}", server.url());
    let mut printed = 0;
    loop {
        // Blocks until the next request, or a day passes and it looks again.
        if !server.await_requests(
            printed + 1,
            Deadline::after(Duration::from_secs(24 * 60 * 60)),
        ) {
            continue;
        }
        for request in server.requests().iter().skip(printed) {
            print!("{}", show(request));
            printed += 1;
        }
    }
}
