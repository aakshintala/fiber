//! The fixture-start table: every Rust `mcp_fixture()` use and jig
//! executable spawn by path and enclosing function, with its exact
//! expected direct-call count. Ordinary sites use one; repeated sites
//! state their count; rows that never call the accessor state zero. A
//! function the table does not name fails the check, and so does a
//! listed function whose call count differs from its row.

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
    /// How many direct `mcp_fixture()` calls the function holds.
    pub(crate) expected: usize,
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
    expected: usize,
) -> Site {
    Site {
        path,
        item,
        kind,
        marker,
        scope,
        expected,
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
        0, // the accessor definition itself is not a call
    ),
    // The warmer itself: the shared once-per-process registry and the
    // guarded starts that call it.
    site(
        "crates/mcp/src/test_support.rs",
        "specs_use_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "warm_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "spec",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_with_images_within",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_expect",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/mcp/src/test_support.rs",
        "start_result",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        2, // two direct calls: the command check and the warm
    ),
    // Part 1: direct starts through the shared helper before their own
    // deadlines.
    site(
        "crates/mcp/src/server_tests.rs",
        "endless_pages_end_at_the_startup_deadline",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/server_tests.rs",
        "lingering",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/server_tests.rs",
        "a_server_that_stops_reading_stdin_ends_every_call_gone",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "servers_stop_at_once",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "secret_wrapper",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "a_second_restart_mid_start_coalesces_on_the_first_attempt",
        Kind::Warmed,
        "Setup::warm_fixture()",
        BEFORE,
        1,
    ),
    // Part 1: spec and program builders flowing into the guarded
    // shared start.
    site(
        "crates/mcp/src/start_tests.rs",
        "held_spec",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "held_spec_with_reached",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/start_tests.rs",
        "run_program",
        Kind::Program,
        "Program {",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "refusing_run",
        Kind::Program,
        "Program {",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/sync/tests.rs",
        "refusing_spec",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
        1,
    ),
    // Part 1: path readers that never exec the fixture.
    site(
        "crates/mcp/src/slot/tests.rs",
        "cached_names",
        Kind::Reader,
        "cache::read(",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "cached_prompts",
        Kind::Reader,
        "cache::read(",
        BEFORE,
        1,
    ),
    site(
        "crates/mcp/src/slot/tests.rs",
        "a_call_to_a_removed_tool_fails_without_calling_and_updates_the_cache",
        Kind::Spec,
        "setup.spec(",
        BEFORE,
        1,
    ),
    // Part 1: the main unit-test binary warms before its setup waits.
    site(
        "crates/main/src/test_support.rs",
        "warm_mcp_fixture_once",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "reload_appends_a_started_servers_prompts_while_keeping_every_startup_row",
        Kind::Warmed,
        "Fixture::with_servers(",
        BEFORE,
        1,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "a_second_reload_retains_the_first_reloads_prompt_rows_for_kept_servers",
        Kind::Warmed,
        "Fixture::with_servers(",
        BEFORE,
        1,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "reload_routes_a_replaced_servers_kept_prompt_through_the_replacement",
        Kind::Warmed,
        "warm_mcp_fixture_once();",
        BEFORE,
        0, // warms once up front; the fixture itself rides through `entry`
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "entry",
        Kind::Warmed,
        "crate::test_support::warm_mcp_fixture_once();",
        IN_FILE,
        1,
    ),
    site(
        "crates/main/src/reload_tests.rs",
        "a_fake_server_entry_captures_stderr_in_its_directory",
        Kind::Reader,
        "FakeServer::new(",
        BEFORE,
        1,
    ),
    site(
        "crates/main/src/mcp_elicit_tests.rs",
        "an_unanswered_elicitation_is_declined_at_the_calls_deadline_then_the_call_times_out",
        Kind::Warmed,
        "warm_mcp_fixture_once()",
        BEFORE,
        1,
    ),
    // Part 1: the main integration binary warms before its deadline.
    site(
        "crates/main/tests/support/mod.rs",
        "new_with_mcp_fixture",
        Kind::Warmer,
        "fn warm_once",
        IN_FILE,
        1,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "list_shows_a_ready_a_failed_and_a_needs_login_server",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "reconnect_restarts_the_failed_server",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_view.rs",
        "two_quick_reconnects_spawn_the_server_once",
        Kind::Warmed,
        "new_with_mcp_fixture()",
        BEFORE,
        2, // two direct calls: the wrapper script and the ready server
    ),
    // Later parts: the remaining main integration binaries keep their
    // explicit rows until those parts warm their starts.
    site(
        "crates/main/tests/reload.rs",
        "mcp_entry",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/diagnostics.rs",
        "mcp_fixture",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "configure_fx",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_repositorys_server_is_not_started",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_persons_empty_env_does_not_mask_a_repositorys_entry",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp.rs",
        "a_required_server_that_fails_to_start_exits_before_the_session",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_elicit.rs",
        "configure_fx",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_commands.rs",
        "a_repo_add_is_approved_here_and_skipped_in_a_fresh_home",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_timing.rs",
        "stdio_server",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/mcp_timing.rs",
        "approved_repository_start_times_connect_and_skipped_writes_none",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/picture_timing.rs",
        "mcp_configure",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/tool_search.rs",
        "fx_server",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/session_command.rs",
        "prompt_fixture",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/repository_servers.rs",
        "declare",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    // Never warmed: test-run copies served through the interpreter, per
    // the file-execution rule.
    site(
        "crates/main/tests/repository_offer.rs",
        "declare_db",
        Kind::Reader,
        "fs::read(",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/repository_offer.rs",
        "declare_early",
        Kind::Reader,
        "fs::read(",
        BEFORE,
        1,
    ),
    // Part 3: the process-extension wrappers warm in a later part.
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_gated_fx",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_plain_fx",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "configure_held_fx",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    // Part 4: the fakes crate's own direct spawn and jig command.
    site(
        "crates/fakes/src/fixture_tests.rs",
        "fixture",
        Kind::Later,
        "",
        BEFORE,
        1,
    ),
    site(
        "crates/fakes/src/process_extension.rs",
        "command",
        Kind::JigWarmer,
        "warm_up(",
        BODY,
        0, // a jig-warmer row never calls the fixture accessor
    ),
    // Jig binaries already warmed under the existing test-process
    // model: each site warms its binary before its own exchange.
    site(
        "crates/fakes/tests/provider_server_jig.rs",
        "it_serves_one_fixture_and_prints_the_request_it_received",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "startup_captures_each_json_line_exactly",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "a_hold_is_acknowledged_then_released_before_the_join",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "a_protocol_peer_acknowledges_each_hold_before_release",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "the_fixture_usage_error_names_its_modes",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/fakes/src/process_extension_tests.rs",
        "finish_fails_when_the_fixture_exits_unsuccessfully",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/provider/tests/record_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/provider/tests/decode_jig.rs",
        "decode",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/tui/tests/paging_open.rs",
        "new",
        Kind::Jig,
        "warm_up(",
        BODY,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/loop/tests/review_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    // The direct spawn below runs after two `run_jig` exchanges warmed
    // the binary in this same test process.
    site(
        "crates/loop/tests/review_jig.rs",
        "bad_argv_and_notes_fail_through_the_binary",
        Kind::Jig,
        "run_jig(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
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
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/loop/tests/turn_jig.rs",
        "turn",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/cli/tests/process_extension_cases.rs",
        "package",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/main/tests/process_extensions.rs",
        "process_with",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/extensions/src/process_tests.rs",
        "start_with_services_on_manifest",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/extensions/src/case/call_tests.rs",
        "install_process_package",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/extensions/src/package_tests.rs",
        "sessionless_provider_declarations_list_registered_functions",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/extensions/tests/process_activation.rs",
        "install_process",
        Kind::FixtureUser,
        "ProcessFixture::",
        BEFORE,
        0, // a fixture-user row never calls the fixture accessor
    ),
    site(
        "crates/extensions/tests/pin_check_jig.rs",
        "pin_check",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/config/tests/resolve_jig.rs",
        "resolve",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/log/tests/dump_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/log/tests/check_jig.rs",
        "run_jig",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
    site(
        "crates/tools/tests/call_jig.rs",
        "call",
        Kind::Jig,
        "warm_up(",
        BEFORE,
        0, // a jig row never calls the fixture accessor
    ),
];
