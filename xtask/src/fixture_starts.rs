//! The fixture-start ratchet (`docs/testing/fakes.md`, "Jigs"): every
//! first exec of the checked-in MCP fixture
//! goes through `fakes::warm_up`, or through a start helper that calls
//! it, because the first exec of a freshly linked binary can stall for
//! seconds on macOS. Every Rust `mcp_fixture()` use and jig executable
//! spawn must match a named path/function row below; an unlisted
//! occurrence fails, so a new fixture start cannot enter without an
//! explicit site row.
//!
//! A warmed row names the helper call that must still be present before
//! the fixture execution; removing it fails. Direct `Server::start` rows
//! require the preceding `Setup::warm_fixture()` call, and indirect
//! Bash/flaky wrapper rows require the named helper or test to warm
//! before starting. Path-reader rows never exec the fixture: they pin
//! the reading call and fail when a start enters the same function.
//! Later-part rows name the binaries Parts 2-4 warm; they stay explicit
//! until those parts warm their starts. The shared-start guard itself is
//! proven by `every_start_helper_warms_the_fixture_before_its_deadline`
//! in `crates/mcp/src/start_tests.rs`, which this check does not
//! re-prove: these rows pin the sites that guard covers.

use crate::rules::RustFile;

/// Where the row's marker must appear: between the function definition
/// and the fixture use (the warm precedes the execution), anywhere in
/// the function body (a warm later in the body still covers the process),
/// or anywhere in the file as a trimmed code line (a wrapper whose
/// callers warm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    BeforeUse,
    Body,
    File,
}

/// One named fixture or jig site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Site {
    /// Repository-relative path, such as `crates/mcp/src/start_tests.rs`.
    pub(crate) path: &'static str,
    /// The enclosing function, such as `servers_stop_at_once`.
    pub(crate) item: &'static str,
    /// What the site is.
    pub(crate) kind: Kind,
    /// The helper call that must still be present (warmed rows only).
    pub(crate) marker: &'static str,
    /// Where the marker must appear.
    pub(crate) scope: Scope,
}

/// What a site is: a warmed start, a spec or program builder flowing
/// into the guarded shared start, a path reader that never execs, a
/// later part, the warmer itself, the pure path definition, or a jig
/// site already warmed under the existing test-process model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Warmed,
    Spec,
    Program,
    Reader,
    Later,
    Warmer,
    Def,
    Jig,
    FixtureUser,
    JigWarmer,
}

const fn site(
    path: &'static str,
    item: &'static str,
    kind: Kind,
    marker: &'static str,
    scope: Scope,
) -> Site {
    Site {
        path,
        item,
        kind,
        marker,
        scope,
    }
}

const BEFORE: Scope = Scope::BeforeUse;
const BODY: Scope = Scope::Body;
const IN_FILE: Scope = Scope::File;

/// Every fixture use and jig spawn in the tree, by path and function.
/// A function the table does not name fails the check.
pub(crate) const SITES: &[Site] = &[
    // The pure path accessor stays pure: it never warms, so path-only
    // callers pay for no fixture exec.
    site(
        "crates/fakes/src/lib.rs",
        "mcp_fixture",
        Kind::Def,
        "warm_up",
        BEFORE,
    ),
    // The warmer itself: the shared once-per-process registry and the
    // guarded starts that call it.
    site(
        "crates/mcp/src/test_support.rs",
        "specs_use_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "warm_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "spec",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_with_images_within",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_expect",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_result",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    // Part 1: direct starts through the shared helper before their own
    // deadlines.
    site(
        "crates/mcp/src/server_tests.rs",
        "endless_pages_end_at_the_startup_deadline",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    site(
        "crates/mcp/src/server_tests.rs",
        "lingering",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    site(
        "crates/mcp/src/server_tests.rs",
        "a_server_that_stops_reading_stdin_ends_every_call_gone",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "servers_stop_at_once",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "secret_wrapper",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "a_second_restart_mid_start_coalesces_on_the_first_attempt",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
    ),
    // Part 1: spec and program builders flowing into the guarded
    // shared start.
    site(
        "crates/mcp/src/start_tests.rs",
        "held_spec",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "held_spec_with_reached",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "run_program",
        Kind::Program,
        "Program {",
        BEFORE,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "refusing_run",
        Kind::Program,
        "Program {",
        BEFORE,
    ),
    site(
        "crates/mcp/src/sync/tests.rs",
        "refusing_spec",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
    ),
    // Part 1: path readers that never exec the fixture.
    site(
        "crates/mcp/src/slot/tests.rs",
        "cached_names",
        Kind::Reader,
        "cache::read(",
        BEFORE,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "cached_prompts",
        Kind::Reader,
        "cache::read(",
        BEFORE,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "a_call_to_a_removed_tool_fails_without_calling_and_updates_the_cache",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
    ),
    // Part 1: the main unit-test binary warms before its setup waits.
    site(
        "crates/main/src/test_support.rs",
        "warm_mcp_fixture_once",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "reload_appends_a_started_servers_prompts_while_keeping_every_startup_row",
        Kind::Warmed,
        "Fixture::with_servers(",
        BEFORE,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "a_second_reload_retains_the_first_reloads_prompt_rows_for_kept_servers",
        Kind::Warmed,
        "Fixture::with_servers(",
        BEFORE,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "reload_routes_a_replaced_servers_kept_prompt_through_the_replacement",
        Kind::Warmed,
        "warm_mcp_fixture_once();",
        BEFORE,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "entry",
        Kind::Warmed,
        "crate::test_support::warm_mcp_fixture_once();",
        IN_FILE,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "a_fake_server_entry_captures_stderr_in_its_directory",
        Kind::Reader,
        "FakeServer::new(",
        BEFORE,
    ),
    site(
        "crates/main/src/mcp_elicit_tests.rs",
        "an_unanswered_elicitation_is_declined_at_the_calls_deadline_then_the_call_times_out",
        Kind::Warmed,
        "warm_mcp_fixture_once()",
        BEFORE,
    ),
    // Part 1: the main integration binary warms before its deadline.
    site(
        "crates/main/tests/support/mod.rs",
        "new_with_mcp_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "list_shows_a_ready_a_failed_and_a_needs_login_server",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "reconnect_restarts_the_failed_server",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "two_quick_reconnects_spawn_the_server_once",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
    ),
    // Later parts: the remaining main integration binaries keep their
    // explicit rows until those parts warm their starts.
    site(
        "crates/main/tests/reload.rs",
        "mcp_entry",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/diagnostics.rs",
        "mcp_fixture",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "configure_fx",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_repositorys_server_is_not_started",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_persons_empty_env_does_not_mask_a_repositorys_entry",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_required_server_that_fails_to_start_exits_before_the_session",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_elicit.rs",
        "configure_fx",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_commands.rs",
        "a_repo_add_is_approved_here_and_skipped_in_a_fresh_home",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_timing.rs",
        "stdio_server",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/mcp_timing.rs",
        "approved_repository_start_times_connect_and_skipped_writes_none",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/picture_timing.rs",
        "mcp_configure",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/tool_search.rs",
        "fx_server",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/session_command.rs",
        "prompt_fixture",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/repository_servers.rs",
        "declare",
        Kind::Later,
        "",
        BEFORE,
    ),
    // Never warmed: test-run copies served through the interpreter, per
    // the file-execution rule.
    site(
        "crates/main/tests/repository_offer.rs",
        "declare_db",
        Kind::Reader,
        "fs::read(",
        BEFORE,
    ),
    site(
        "crates/main/tests/repository_offer.rs",
        "declare_early",
        Kind::Reader,
        "fs::read(",
        BEFORE,
    ),
    // Part 3: the process-extension wrappers warm in a later part.
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_gated_fx",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_plain_fx",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_held_fx",
        Kind::Later,
        "",
        BEFORE,
    ),
    // Part 4: the fakes crate's own direct spawn and jig command.
    site(
        "crates/fakes/src/fixture_tests.rs",
        "fixture",
        Kind::Later,
        "",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension.rs",
        "command",
        Kind::JigWarmer,
        "warm_up(",
        BODY,
    ),
    // Jig binaries already warmed under the existing test-process
    // model: each site warms its binary before its own exchange.
    site(
        "crates/fakes/tests/provider_server_jig.rs",
        "it_serves_one_fixture_and_prints_the_request_it_received",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "startup_captures_each_json_line_exactly",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "a_hold_is_acknowledged_then_released_before_the_join",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "a_protocol_peer_acknowledges_each_hold_before_release",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "the_fixture_usage_error_names_its_modes",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "finish_fails_when_the_fixture_exits_unsuccessfully",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/provider/tests/record_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/provider/tests/decode_jig.rs",
        "decode",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/tui/tests/paging_open.rs",
        "new",
        Kind::Jig,
        "warm_up(",
        BODY,
    ),
    site(
        "crates/loop/tests/review_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    // The direct spawn below runs after two `run_jig` exchanges warmed
    // the binary in this same test process.
    site(
        "crates/loop/tests/review_jig.rs",
        "bad_argv_and_notes_fail_through_the_binary",
        Kind::Jig,
        "run_jig(",
        BEFORE,
    ),
    // The held spawn reports its pid on a ready FIFO and the parent
    // reads it under `START_BOUND`: that bounded wait, not a warm-up,
    // absorbs the first-exec stall.
    site(
        "crates/loop/tests/review_jig.rs",
        "spawn_held_jig",
        Kind::Jig,
        "Ready::new(",
        BEFORE,
    ),
    site(
        "crates/loop/tests/turn_jig.rs",
        "turn",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/cli/tests/process_extension_cases.rs",
        "package",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "process_with",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/extensions/src/process_tests.rs",
        "start_with_services_on_manifest",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/extensions/src/case/call_tests.rs",
        "install_process_package",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/extensions/src/package_tests.rs",
        "sessionless_provider_declarations_list_registered_functions",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/extensions/tests/process_activation.rs",
        "install_process",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
    ),
    site(
        "crates/extensions/tests/pin_check_jig.rs",
        "pin_check",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/config/tests/resolve_jig.rs",
        "resolve",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/log/tests/dump_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/log/tests/check_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
    site(
        "crates/tools/tests/call_jig.rs",
        "call",
        Kind::Jig,
        "warm_up(",
        BEFORE,
    ),
];

/// Starts a test binary may not make: a reader that grew one fails.
const STARTS: &[&str] = &[
    "Server::start",
    ".start(",
    "start_expect(",
    "relaunch(",
    "prepare(",
    "Command::new(",
];

/// One failure per unlisted use, missing marker, or reader that grew
/// a start, in path then line order.
pub(crate) fn fixture_starts(files: &[RustFile]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut by_path: std::collections::BTreeMap<&str, &RustFile> =
        std::collections::BTreeMap::new();
    for file in files {
        by_path.insert(file.path.as_str(), file);
    }
    for file in files {
        let lines: Vec<&str> = file.source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("use ")
                || trimmed.starts_with("pub use ")
                || trimmed.starts_with("//")
                || trimmed.starts_with('*')
            {
                continue;
            }
            // A mention inside a string literal is documentation, not a
            // start: the checker's own table names every site that way.
            let code = strip_literals(line);
            let is_fixture = code.contains("mcp_fixture()");
            let is_jig = code.contains("example_path()") || code.contains("ProcessFixture");
            if !is_fixture && !is_jig {
                continue;
            }
            if line.contains("fn example_path()") || line.contains("impl ProcessFixture") {
                continue;
            }
            // A type definition names its type without using it.
            let after_pub = trimmed
                .strip_prefix("pub")
                .map(|rest| {
                    let rest = rest.strip_prefix("(crate)").unwrap_or(rest);
                    let rest = rest.strip_prefix("(super)").unwrap_or(rest);
                    let rest = rest.strip_prefix("(self)").unwrap_or(rest);
                    rest.trim_start()
                })
                .unwrap_or(trimmed);
            if after_pub.starts_with("struct ")
                || after_pub.starts_with("enum ")
                || after_pub.starts_with("union ")
                || after_pub.starts_with("trait ")
            {
                continue;
            }
            let Some(fn_line) = enclosing_fn(&lines, index) else {
                failures.push(format!(
                    "{}:{}: fixture use outside any function",
                    file.path,
                    index + 1
                ));
                continue;
            };
            let item = lines.get(fn_line).and_then(|line| fn_name(line));
            let Some(row) = SITES
                .iter()
                .find(|row| row.path == file.path && Some(row.item) == item.as_deref())
            else {
                failures.push(format!(
                    "{}:{}: unlisted fixture start in `{}`; add an exact-site row",
                    file.path,
                    index + 1,
                    item.as_deref().unwrap_or("?"),
                ));
                continue;
            };
            let item_name = item.as_deref().unwrap_or("?");
            if is_jig
                && !is_fixture
                && !matches!(
                    row.kind,
                    Kind::Jig | Kind::FixtureUser | Kind::JigWarmer | Kind::Later
                )
            {
                failures.push(format!(
                    "{}:{}: `{item_name}` is a jig site but its row is not a jig kind",
                    file.path,
                    index + 1
                ));
                continue;
            }
            if is_fixture && matches!(row.kind, Kind::Jig | Kind::FixtureUser | Kind::JigWarmer) {
                failures.push(format!(
                    "{}:{}: `{item_name}` uses the fixture but its row is a jig kind",
                    file.path,
                    index + 1
                ));
                continue;
            }
            match row.kind {
                Kind::Later => {}
                Kind::Warmer => {
                    if !file_contains_code_line(&lines, row.marker) {
                        failures.push(format!(
                            "{}:{}: `{item_name}` lost its warmer `{}`",
                            file.path,
                            index + 1,
                            row.marker
                        ));
                    }
                }
                Kind::Def => {
                    if body(fn_line, &lines)
                        .iter()
                        .any(|body_line| body_line.contains(row.marker))
                    {
                        failures.push(format!(
                            "{}:{}: the pure accessor `{item_name}` grew a `{}` call",
                            file.path,
                            index + 1,
                            row.marker
                        ));
                    }
                }
                Kind::Warmed
                | Kind::Spec
                | Kind::Program
                | Kind::Reader
                | Kind::Jig
                | Kind::FixtureUser
                | Kind::JigWarmer => {
                    let body_lines;
                    let window: &[&str] = match row.scope {
                        Scope::BeforeUse => lines.get(fn_line..=index).unwrap_or(&[]),
                        Scope::Body => {
                            body_lines = body(fn_line, &lines);
                            &body_lines
                        }
                        Scope::File => &lines[..],
                    };
                    let holds = match row.scope {
                        Scope::BeforeUse | Scope::Body => window
                            .iter()
                            .any(|window_line| window_line.contains(row.marker)),
                        Scope::File => file_contains_code_line(&lines, row.marker),
                    };
                    if !holds {
                        failures.push(format!(
                            "{}:{}: `{item_name}` lost its `{}` call before the fixture use",
                            file.path,
                            index + 1,
                            row.marker
                        ));
                    }
                    // A reader never starts: the whole body, not just the
                    // lines above the use, must hold no start.
                    if matches!(row.kind, Kind::Reader)
                        && body(fn_line, &lines)
                            .iter()
                            .any(|body_line| STARTS.iter().any(|start| body_line.contains(start)))
                    {
                        failures.push(format!(
                            "{}:{}: path reader `{item_name}` grew a start",
                            file.path,
                            index + 1
                        ));
                    }
                }
            }
        }
    }
    // Every row names a use that still exists; a renamed function fails
    // here instead of silently dropping its pin.
    for row in SITES {
        let Some(file) = by_path.get(row.path) else {
            failures.push(format!("{}: row names a missing file", row.path));
            continue;
        };
        let lines: Vec<&str> = file.source.lines().collect();
        if !lines
            .iter()
            .any(|line| is_fn_def(line) && fn_name(line).as_deref() == Some(row.item))
        {
            failures.push(format!(
                "{}: row names a missing function `{}`",
                row.path, row.item
            ));
        }
    }
    failures.sort();
    failures
}

/// The definition line index of the function holding `lines[index]`.
fn enclosing_fn(lines: &[&str], index: usize) -> Option<usize> {
    for at in (0..=index).rev() {
        let line = lines.get(at)?;
        if is_fn_def(line) {
            return Some(at);
        }
        if *line == "}" {
            return None;
        }
    }
    None
}

/// Whether the line defines a function: `fn`, `pub fn`, `pub(crate)
/// fn` and `async fn` openers, never `use` lines or comments.
fn is_fn_def(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
        return false;
    }
    if !(trimmed.starts_with("fn ") || trimmed.starts_with("pub") || trimmed.starts_with("async "))
    {
        return false;
    }
    if !trimmed.contains("fn ") || !trimmed.contains('(') {
        return false;
    }
    // A `pub` opener names the function itself, not a later call: the
    // first `fn ` starts the name.
    !trimmed.starts_with("pub") || trimmed.find("fn ").is_some()
}

/// The function name a definition line defines.
fn fn_name(line: &str) -> Option<String> {
    let at = line.find("fn ")? + 3;
    line[at..]
        .split(|cell: char| !cell.is_alphanumeric() && cell != '_')
        .find(|part| !part.is_empty())
        .map(str::to_owned)
}

/// The line without its double-quoted, raw string and char contents: a
/// site named inside documentation is not a start. Escapes stay inside
/// their literal; an unbalanced quote drops the rest of the line. A
/// lifetime is not a char literal: only a quote closed right after its
/// char is skipped.
fn strip_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(cell) = chars.next() {
        if cell == '"' {
            let iter = chars.by_ref();
            while let Some(next) = iter.next() {
                if next == '\\' {
                    iter.next();
                } else if next == '"' {
                    break;
                }
            }
        } else if cell == 'r' && matches!(chars.peek(), Some('#' | '"')) {
            let mut hashes = 0;
            while chars.peek() == Some(&'#') {
                chars.next();
                hashes += 1;
            }
            if chars.peek() == Some(&'"') {
                chars.next();
                let closer: String = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                let mut tail = String::new();
                for next in chars.by_ref() {
                    tail.push(next);
                    if tail.ends_with(&closer) {
                        break;
                    }
                    if tail.len() > line.len() {
                        break;
                    }
                }
            } else {
                out.push(cell);
                for _ in 0..hashes {
                    out.push('#');
                }
            }
        } else if cell == '\'' {
            let rest: String = chars.clone().take(3).collect();
            let mut chars_vec = rest.chars();
            let closed = match (chars_vec.next(), chars_vec.next(), chars_vec.next()) {
                (Some('\\'), Some(_), Some('\'')) => 3,
                (Some(_), Some('\''), _) => 2,
                _ => 0,
            };
            if closed == 0 {
                out.push(cell);
            } else {
                for _ in 0..closed {
                    chars.next();
                }
            }
        } else {
            out.push(cell);
        }
    }
    out
}

/// Whether any trimmed code line starts with `marker`, so source-text
/// self-mentions inside string literals never count as the call.
fn file_contains_code_line(lines: &[&str], marker: &str) -> bool {
    lines
        .iter()
        .any(|line| line.trim_start().starts_with(marker))
}

/// The function body from its definition line through the next
/// column-zero `}` or definition-level opener.
fn body<'a>(start: usize, lines: &'a [&'a str]) -> Vec<&'a str> {
    let mut end = lines.len();
    for (offset, line) in lines.iter().enumerate().skip(start + 1) {
        if *line == "}"
            || line.trim_start().starts_with("#[")
            || is_fn_def(line)
            || line.trim_start().starts_with("impl ")
            || line.trim_start().starts_with("mod ")
        {
            end = offset;
            break;
        }
    }
    lines.get(start..end).map_or_else(Vec::new, <[_]>::to_vec)
}
