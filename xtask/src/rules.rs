//! Checks that the code matches the rules and tables in the docs: the
//! 800-line report (`docs/code-quality.md`, "Size"), the `unsafe` table in
//! `docs/code-quality.md`, the crate list in `docs/dependencies.md`, and
//! that process signals stay in the guarded helpers.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};

use crate::select::is_test_file;

const LINE_CAP: usize = 800;

/// A Rust file in a workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustFile {
    pub(crate) krate: String,
    /// Relative to the repository.
    pub(crate) path: String,
    /// Relative to the crate.
    pub(crate) rel: String,
    pub(crate) source: String,
}

pub(crate) fn over_cap(files: &[RustFile]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !is_test_file(&f.rel))
        .filter_map(|f| {
            let lines = f.source.matches('\n').count();
            (lines > LINE_CAP).then(|| format!("{}: {lines} lines, over {LINE_CAP}", f.path))
        })
        .collect()
}

/// Crates only some workspace members may link.
pub(crate) struct Isolation {
    /// The crates kept out of every other member's normal dependency tree.
    pub(crate) crates: &'static [&'static str],
    /// The members that may link them.
    pub(crate) members: &'static [&'static str],
    /// What a failure says after the crate it found.
    pub(crate) why: &'static str,
}

/// The crates only the image child links (`docs/dependencies.md`, "Crates
/// used only by the image child").
pub(crate) const IMAGE: Isolation = Isolation {
    crates: &["image", "fast_image_resize", "lopdf"],
    members: &["picture", "main"],
    why: "only the image child links image code",
};

/// The crates admitted only for `tui` (`docs/dependencies.md`, "Admitting
/// a crate"): no other Fiber crate depends on them.
pub(crate) const TUI: Isolation = Isolation {
    crates: &["ratatui", "crossterm"],
    members: &["tui", "main"],
    why: "only the terminal links terminal UI code",
};

/// Each failure where a workspace member's normal dependency tree names
/// one of `rule`'s crates. `trees` pairs a member with the output of `cargo
/// tree -p MEMBER -e normal --prefix none`: one line per crate, the crate's
/// name first. `rule`'s members are exempt.
pub(crate) fn leaks(trees: &[(String, String)], rule: &Isolation) -> Vec<String> {
    let mut failures = BTreeSet::new();
    for (member, tree) in trees {
        if rule.members.contains(&member.as_str()) {
            continue;
        }
        for name in tree
            .lines()
            .filter_map(|line| line.split_whitespace().next())
        {
            if rule.crates.contains(&name) {
                failures.insert(format!(
                    "{member}: its normal dependency tree holds {name}; {}",
                    rule.why
                ));
            }
        }
    }
    failures.into_iter().collect()
}

/// The feature node `cargo tree --prefix depth -e features` prints for
/// serde_json's `preserve_order`, without a deduplication marker.
const PRESERVE_ORDER: &str = "serde_json feature \"preserve_order\"";

/// Each failure where the workspace build enables serde_json's
/// `preserve_order` feature, which `docs/prompt-cache.md` never allows.
/// `tree` is the output of `cargo tree --workspace --target all
/// --all-features -e features,no-build --prefix depth -i serde_json`:
/// every line starts with its depth, so the package lines
/// directly under a `preserve_order` node, one level deeper, are the
/// crates that enabled it. Deeper crates only depend on an enabler.
/// A bare node with no crate under it still fails: the feature is on,
/// so the build is already poisoned.
pub(crate) fn preserve_order(tree: &str) -> Vec<String> {
    let mut enablers = BTreeSet::new();
    let mut nodes = 0;
    let mut depth: Option<usize> = None;
    for line in tree.lines() {
        let digits = line.chars().take_while(|c| c.is_numeric()).count();
        let (line_depth, text) = match line[..digits].parse::<usize>() {
            Ok(line_depth) => (line_depth, line[digits..].trim_end().to_owned()),
            Err(_) => {
                depth = None;
                continue;
            }
        };
        let text = text.strip_suffix("(*)").unwrap_or(&text).trim_end();
        if text == PRESERVE_ORDER {
            nodes += 1;
            depth = Some(line_depth);
            continue;
        }
        if depth.is_some_and(|d| line_depth <= d) {
            depth = None;
        }
        if depth.is_some_and(|d| line_depth == d + 1)
            && let Some(name) = text.split_whitespace().next()
        {
            enablers.insert(format!(
                "{name}: enables serde_json feature \"preserve_order\"; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
            ));
        }
    }
    if !enablers.is_empty() {
        return enablers.into_iter().collect();
    }
    if nodes > 0 {
        return vec![
            "serde_json feature \"preserve_order\" is enabled but no crate in the tree names it; serde_json's preserve_order feature is never enabled (docs/prompt-cache.md)"
                .to_owned(),
        ];
    }
    Vec::new()
}

/// Whether Rust `source` uses the `unsafe` keyword. proc-macro2 tokenises
/// it, so comments, literals and doc comments are never read as code.
pub(crate) fn uses_unsafe(source: &str) -> Result<bool, proc_macro2::LexError> {
    fn walk(stream: TokenStream) -> bool {
        stream.into_iter().any(|tree| match tree {
            TokenTree::Ident(ident) => ident == "unsafe",
            TokenTree::Group(group) => walk(group.stream()),
            TokenTree::Punct(_) | TokenTree::Literal(_) => false,
        })
    }
    Ok(walk(source.parse()?))
}

/// The body of the section under `heading`, up to the next heading of the
/// same or a higher level.
pub(crate) fn section<'a>(markdown: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let level = |line: &str| {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        (hashes > 0 && line.get(hashes..).is_some_and(|rest| rest.starts_with(' ')))
            .then_some(hashes)
    };
    let mut lines = markdown.lines();
    let start = lines.by_ref().find_map(|line| {
        let hashes = level(line)?;
        (line.get(hashes..).map(str::trim) == Some(heading)).then_some(hashes)
    })?;
    Some(
        lines
            .take_while(|line| level(line).is_none_or(|l| l > start))
            .collect(),
    )
}

fn is_separator(line: &str) -> bool {
    line.contains('-')
        && line.starts_with('|')
        && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The cells of every table body row in `lines`.
pub(crate) fn table_rows(lines: &[&str]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in lines.iter().map(|l| l.trim()) {
        if is_separator(line) {
            rows.pop(); // the header row
        } else if let Some(inner) = line.strip_prefix('|') {
            let inner = inner.strip_suffix('|').unwrap_or(inner);
            rows.push(
                inner
                    .split('|')
                    .map(|cell| cell.trim().to_owned())
                    .collect(),
            );
        }
    }
    rows
}

fn unticked(cell: &str) -> String {
    cell.trim_matches('`').to_owned()
}

/// (crate, file) pairs the `unsafe` table lists.
pub(crate) fn unsafe_table(code_quality: &str) -> Result<BTreeSet<(String, String)>, String> {
    let lines =
        section(code_quality, "`unsafe`").ok_or("docs/code-quality.md has no `unsafe` section")?;
    Ok(table_rows(&lines)
        .iter()
        .filter_map(|cells| match cells.as_slice() {
            [krate, file, ..] if krate != "none yet" => Some((unticked(krate), unticked(file))),
            _ => None,
        })
        .collect())
}

pub(crate) fn unsafe_mismatches(
    files: &[RustFile],
    code_quality: &str,
) -> Result<Vec<String>, String> {
    let mut used = BTreeSet::new();
    for f in files {
        // A file that does not tokenise could hide `unsafe`, so it fails the
        // check rather than being skipped.
        if uses_unsafe(&f.source)
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?
        {
            used.insert((f.krate.clone(), f.path.clone()));
        }
    }
    let listed = unsafe_table(code_quality)?;
    let mut failures: Vec<String> = used
        .difference(&listed)
        .map(|(_, path)| {
            format!("{path}: uses unsafe, but the table in docs/code-quality.md does not list it")
        })
        .collect();
    failures.extend(listed.difference(&used).map(|(krate, path)| {
        format!("{path}: listed for {krate} in docs/code-quality.md, but uses no unsafe")
    }));
    Ok(failures)
}

/// Files that may send a process signal: a trust list, not a per-file
/// test seam. An entry must refuse an id of 1 or less before anything
/// runs: group -1 reaches every process the user owns.
const SIGNAL_ALLOWLIST: &[&str] = &[
    "crates/fakes/src/process_group.rs",
    "crates/main/src/switch/read.rs",
    "crates/support/src/group.rs",
];

pub(crate) fn signal_sites(files: &[RustFile]) -> Vec<String> {
    let mut failures = Vec::new();
    for f in files {
        if SIGNAL_ALLOWLIST.contains(&f.path.as_str()) {
            continue;
        }
        // A file that does not tokenise could hide a signal, so it fails
        // the check rather than being skipped.
        let stream: TokenStream = match f.source.parse() {
            Ok(stream) => stream,
            Err(e) => {
                failures.push(format!("{}: does not tokenise as Rust: {e}", f.path));
                continue;
            }
        };
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        walk_tokens(&f.path, &trees, &mut failures);
    }
    failures
}

/// Each failure where `trees` holds a process signal. Tokens carry no
/// comments, so only code and string contents count; a bare `kill` names
/// nothing on its own. Groups recurse, so signals nested in bodies count
/// with the line where their first token sits.
fn walk_tokens(path: &str, trees: &[TokenTree], failures: &mut Vec<String>) {
    let mut i = 0;
    while let Some(tree) = trees.get(i) {
        if let Some(pattern) = signal_at(trees, i) {
            let line = tree.span().start().line;
            failures.push(format!("{path}:{line}: {pattern}"));
        }
        if let TokenTree::Group(group) = tree {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            walk_tokens(path, &inner, failures);
        }
        i += 1;
    }
}

/// The signal pattern starting at `trees[i]`, if any: an ident naming a
/// signal helper, a path to one, or a string holding a shell fragment with
/// `kill` beside its operand. A shell spelling beside no `kill` is another
/// command's operand, so the string must hold both.
fn signal_at(trees: &[TokenTree], i: usize) -> Option<&'static str> {
    let tree = trees.get(i)?;
    match tree {
        TokenTree::Ident(name) => {
            let name = name.to_string();
            if name.contains("kill_process_group") {
                Some("kill_process_group")
            } else if name.contains("killpg") {
                Some("killpg")
            } else if name.ends_with("kill_process")
                && matches!(trees.get(i + 1), Some(TokenTree::Group(_)))
            {
                Some("kill_process(")
            } else if name == "Command"
                && is_punct(trees.get(i + 1), ':')
                && is_punct(trees.get(i + 2), ':')
                && is_ident(trees.get(i + 3), "new")
                && is_kill_literal(trees.get(i + 4))
            {
                Some("Command::new(\"kill\")")
            } else if name == "libc"
                && is_punct(trees.get(i + 1), ':')
                && is_punct(trees.get(i + 2), ':')
                && is_ident(trees.get(i + 3), "kill")
            {
                Some("libc::kill")
            } else if name == "nix"
                && is_punct(trees.get(i + 1), ':')
                && is_punct(trees.get(i + 2), ':')
                && is_ident(trees.get(i + 3), "sys")
                && is_punct(trees.get(i + 4), ':')
                && is_punct(trees.get(i + 5), ':')
                && is_ident(trees.get(i + 6), "signal")
            {
                Some("nix::sys::signal")
            } else {
                None
            }
        }
        TokenTree::Literal(literal) => {
            let spelling = literal.to_string();
            if spelling.contains("-- -") && spelling.contains("kill") {
                Some("-- -")
            } else if spelling.contains("\"-$") && spelling.contains("kill") {
                Some("\"-$")
            } else {
                None
            }
        }
        TokenTree::Punct(_) | TokenTree::Group(_) => None,
    }
}

fn is_punct(tree: Option<&TokenTree>, ch: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(punct)) if punct.as_char() == ch)
}

fn is_ident(tree: Option<&TokenTree>, word: &str) -> bool {
    matches!(tree, Some(TokenTree::Ident(ident)) if ident == word)
}

/// One kept raw fake-clock advance outside `crates/fakes`: the checker
/// reports every raw `FakeClock::advance` or `advance_marked` call it can
/// prove, and each proved call needs exactly one of these entries.
pub(crate) struct RawAdvanceAllowance {
    pub(crate) path: &'static str,
    pub(crate) function: &'static str,
    pub(crate) occurrence: u32,
    pub(crate) reason: &'static str,
    pub(crate) ruling: &'static str,
}

macro_rules! raw_advance {
    ($path:literal, $function:literal, $occurrence:literal, $reason:expr, $ruling:literal) => {
        RawAdvanceAllowance {
            path: $path,
            function: $function,
            occurrence: $occurrence,
            reason: $reason,
            ruling: $ruling,
        }
    };
}

/// The keep-raw reasons (`docs/testing.md`, "Waits and timeouts").
pub(crate) const NO_PARK: &str = "no code under test waits on the clock";
pub(crate) const PANIC_RELEASE: &str = "panic-handler release that re-raises the original payload";
pub(crate) const POLL_PAUSE: &str =
    "poll-pause jump: proven parked at its poll, crossing only poll pauses and the deadline";
pub(crate) const LATE_WAKE: &str =
    "late wake: one advance past a park proven at its exact instant just before";
pub(crate) const STALL_DRIVER: &str =
    "stall driver: a shared test-support driver whose callers own the bounds";

/// Whether a keep-raw allowance cites one of the five documented
/// reason/ruling pairs (`docs/testing.md`, "Waits and timeouts"): a matching
/// site with any other pair still fails, so a wrong ruling cannot silence
/// the ban.
fn allowance_pair_valid(reason: &str, ruling: &str) -> bool {
    (reason == NO_PARK && ruling == "6179406086")
        || (reason == PANIC_RELEASE && ruling == "6203478697")
        || (reason == POLL_PAUSE && ruling == "6260728504")
        || (reason == LATE_WAKE && ruling == "6261259790")
        || (reason == STALL_DRIVER && ruling == "6263191610")
}

/// Every kept raw `FakeClock` advance outside `crates/fakes` the checker
/// proves, emitted by a one-off inventory script and hand-checked at each
/// non-no-park reason. A call the checker cannot prove carries no entry:
/// an ambiguous helper name, a method-call result, or a field of an
/// unproved base (docs/testing.md:254-257).
#[rustfmt::skip]
pub(crate) const RAW_ADVANCE_ALLOWANCES: &[RawAdvanceAllowance] = &[
    raw_advance!("crates/bench/examples/bench/main_tests.rs", "a_stall_shifts_one_sample_not_a_side", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/bench/examples/bench/run_tests.rs", "a_startup_timing_starts_at_the_clock_read_just_before_the_spawn", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/bench/examples/bench/wheel_tests.rs", "the_send_stamp_is_read_before_the_write_it_precedes", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/bench/examples/bench/wheel_tests.rs", "the_send_stamp_is_read_before_the_write_it_precedes", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/cli/src/hub_service_tests.rs", "a_hung_command_is_killed_and_reaped_past_its_wait", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/cli/src/hub_service_tests.rs", "an_exited_child_with_a_lingering_descendant_is_reaped_within_its_wait", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/cli/src/sessions_search_tests.rs", "session", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/cli/src/sessions_search_tests.rs", "control_characters_in_paths_stay_on_their_lines", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/cli/src/stop_tests.rs", "a_silent_hub_fails_at_the_deadline", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/cli/src/stop_tests.rs", "a_failed_close_read_is_lost_not_late", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/doors/src/isolation/timing_tests.rs", "create_opens_one_stamped_line_and_a_second_open_writes_nothing", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/doors/src/isolation/timing_tests.rs", "recreation_stamp_excludes_the_lock_claim_and_the_downgrade", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/doors/src/isolation/timing_tests.rs", "recreation_stamp_excludes_the_lock_claim_and_the_downgrade", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/git_tests.rs", "a_stalled_diff_fails_at_the_git_deadline", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/extensions/src/hooks_tests.rs", "process_watcher_uses_the_session_clock", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "quota_now_refetches_only_after_the_freshness_boundary", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "quota_now_refetches_only_after_the_freshness_boundary", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "quota_cached_never_starts_a_fetch_even_after_the_freshness_boundary", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_fetch_times_out_at_exactly_eight_seconds", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_failed_fetch_is_unreachable_with_the_last_value_and_its_age", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_window_reads_as_reset_once_its_reset_time_passes", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_window_reads_as_reset_once_its_reset_time_passes", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "an_unreachable_last_value_maps_a_passed_reset_time", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_model_call_refetches_only_past_five_minutes", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "a_model_call_refetches_only_past_five_minutes", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "the_observed_factory_respects_freshness_for_its_bound_pair", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_provider/quota_tests.rs", "the_observed_factory_respects_freshness_for_its_bound_pair", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/lua_tests.rs", "the_thread_quits_after_the_running_callback_once_stopped", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/prepare_tests.rs", "a_stalled_install_step_is_stopped_at_its_deadline", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/extensions/src/repository/declared_tests.rs", "stalled_ls_files_child", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/extensions/src/repository/store_tests.rs", "a_stalled_install_step_approves_nothing_and_leaves_no_copy", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/extensions/src/tui_vm_tests.rs", "loading_deadline_starts_before_source_read_and_precedes_read_error", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/src/tui_vm_tests.rs", "call_advancing", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/common/mod.rs", "drive", 1, STALL_DRIVER, "6263191610"),
    raw_advance!("crates/extensions/tests/credential_headers.rs", "a_refresh_replaces_the_token_and_its_headers_together", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/credential_headers.rs", "a_completed_call_with_one_token_and_sixteen_headers_keeps_only_sixteen_history_values", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/credential_pair.rs", "a_background_refresh_of_one_pair_leaves_another_pair_alone", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/lazy_commands.rs", "stalled_first_run", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/extensions/tests/lua.rs", "a_callback_the_hook_cannot_stop_abandons_the_vm_and_the_session_survives", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/extensions/tests/lua_provider.rs", "a_token_within_five_minutes_of_expiry_is_refreshed_off_the_request_path", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/lua_provider.rs", "sign_sees_the_extension_clocks_time_in_unix_seconds", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/extensions/tests/oauth.rs", "poll_that_times_out_while_sleeping_is_a_timeout", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/hub/src/connection_tests.rs", "the_idle_timer_starts_when_the_open_count_reaches_zero", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/connection_tests.rs", "the_idle_timer_starts_when_the_open_count_reaches_zero", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/connection_tests.rs", "rollback_removes_the_count_and_restarts_the_timer_at_zero", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/connection_tests.rs", "rollback_removes_the_count_and_restarts_the_timer_at_zero", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/idle_tests.rs", "an_accept_after_the_exit_claim_gets_eof_without_hello", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/pairing_tests.rs", "authenticate_updates_last_connected", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/resume_tests.rs", "a_command_whose_write_to_the_exiting_relay_fails_follows_the_rerouted_one", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/sessions_stop_tests.rs", "an_expired_end_with_no_stop_finishes_the_partial_round", 1, LATE_WAKE, "6261259790"),
    raw_advance!("crates/hub/src/sessions_stop_tests.rs", "a_dead_peer_abandons_the_write_as_gone", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/sessions_stop_tests.rs", "a_newline_terminated_ack_answers_without_a_close", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/sessions_stop_tests.rs", "buffered_events_plus_the_acknowledgement_answer_at_once", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/sessions_stop_tests.rs", "an_unconnected_socket_is_gone_not_waited_on", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/timing_tests.rs", "a_held_summary_handshake_ends_the_timer_at_bind", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/hub/src/timing_tests.rs", "a_failed_transfer_is_stamped_for_the_next_drain", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/jobs/src/delegate/run_tests.rs", "wake_until_draining", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/jobs/src/delegate/run_tests.rs", "the_watch_stats_the_log_once_per_poll_interval", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/jobs/src/timing_tests.rs", "through_launch", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/src/diag_tests.rs", "timed_text", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/src/search/session_tests.rs", "add", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/src/search/session_tests.rs", "add_action", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/src/search_tests.rs", "session", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/src/search_timing_tests.rs", "timed", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project", 4, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project", 5, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_never_returns_session_search_own_calls_or_results", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_never_returns_session_search_own_calls_or_results", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_never_returns_session_search_own_calls_or_results", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/search.rs", "a_search_never_returns_session_search_own_calls_or_results", 4, NO_PARK, "6179406086"),
    raw_advance!("crates/log/tests/writing.rs", "a_line_carries_the_envelope_the_log_fills_in", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/loop/src/inbox/session_message_tests.rs", "a_hook_parked_past_its_registered_timeout_answers_hook_failed", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/loop/src/retry_tests.rs", "now", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/loop/tests/support/mod.rs", "run", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/loop/tests/warm.rs", "call", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "timeout_leaves_a_stubborn_group_listed_at_its_bound", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "timeout_with_a_live_leader_answers_at_the_bound_without_the_reap", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "a_key_finished_after_its_timeout_is_refused", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "settle_withheld_reap_spends_the_bound_with_the_group_listed", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "settle_withheld_reap_spends_the_bound_with_the_group_listed", 2, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "settle_retries_a_perm_group_until_the_bound_then_fails", 1, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "settle_retries_a_perm_group_until_the_bound_then_fails", 2, POLL_PAUSE, "6260728504"),
    raw_advance!("crates/main/src/switch/read_tests.rs", "an_empty_shutdown_answers_true_past_its_end", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/src/tui_extensions_tests.rs", "render_deadline_reports_typed_extension_failure_and_switches_off", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/look.rs", "a_reader_past_its_deadline_ends", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/look.rs", "until_hub_exits_with_panics_naming_what_when_the_hub_outlives_the_deadline", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/mcp_elicit.rs", "read_fifo_past_a_spent_deadline_times_out", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/mcp_elicit.rs", "a_hold_that_never_reaches_the_server_is_named", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/rail_focus.rs", "hint_lift_reports_which_condition_was_false_on_a_spent_deadline", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/rail_focus.rs", "hint_lift_reports_which_condition_was_false_on_a_spent_deadline", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/rail_focus.rs", "spent_wait_message_with_a_painted_first_card", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/session_search.rs", "session", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/sessions_search.rs", "session", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/support/pty.rs", "wait_screen_panics_with_the_checked_grid_and_names_the_deadline_left", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/main/tests/support/pty.rs", "wait_screen_leading_puts_the_lead_before_the_wait_and_the_screen", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/mcp/src/login_tests.rs", "drive_releasing_on_panic", 1, PANIC_RELEASE, "6203478697"),
    raw_advance!("crates/mcp/src/login_tests.rs", "a_reset_in_the_drain_breaks_the_drain", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "milestones_count_from_the_send_not_from_pre_send_work", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "milestones_count_from_the_send_not_from_pre_send_work", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "milestones_count_from_the_send_not_from_pre_send_work", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "milestones_count_from_the_send_not_from_pre_send_work", 4, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "milestones_count_from_the_send_not_from_pre_send_work", 5, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "a_backwards_clock_contributes_zero", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "the_first_latch_wins", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "the_first_latch_wins", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/call_tests.rs", "finish_writes_once", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/http_tests.rs", "an_observed_open_with_timing_times_one_connector_scope", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/http_tests.rs", "an_observed_run_with_timing_times_one_scope_and_reads_the_reply", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/provider/src/http_tests.rs", "the_net_scope_ends_while_the_response_is_held", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "left_is_zero_from_the_waits_on_and_never_renews", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "left_is_zero_from_the_waits_on_and_never_renews", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "left_is_zero_from_the_waits_on_and_never_renews", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "left_is_zero_from_the_waits_on_and_never_renews", 4, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "left_is_zero_from_the_waits_on_and_never_renews", 5, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "no_time_left_gives_zero_and_never_renews", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "no_time_left_gives_zero_and_never_renews", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "cleanup_keeps_its_reserve_after_the_waits_end", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "cleanup_keeps_its_reserve_after_the_waits_end", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "cleanup_keeps_its_reserve_after_the_waits_end", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "cleanup_phase_left_is_cleanup_at_three_instants", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/support/src/deadline_tests.rs", "cleanup_phase_left_is_cleanup_at_three_instants", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/job_timing_tests.rs", "through_open", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/monitor_tests.rs", "flood", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "the_first_change_goes_out_at_once_and_later_ones_collapse", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "the_first_change_goes_out_at_once_and_later_ones_collapse", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "a_large_delta_pushes_the_next_one_out_by_its_size", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "a_large_delta_pushes_the_next_one_out_by_its_size", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "an_incomplete_character_waits_for_its_other_half", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "a_dropped_queue_discards_the_carried_tail", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "a_kept_queue_keeps_the_carried_tail", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tools/src/shell/output_tests.rs", "held_text_across_not_yet_due_passes_is_capped_at_the_newest_64_kib", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "hub_session_not_found_refusal_retries_the_open_delegate_subscribe", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_view_refused_full_is_resent_once_at_the_retry", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_view_refused_full_is_resent_once_at_the_retry", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_view_refused_full_is_resent_once_at_the_retry", 3, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_card_refused_summary_is_resent_likewise", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "no_resend_after_the_parent_lists_the_job_completed", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "no_resend_with_the_link_down", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_refusal_with_another_code_is_never_resent_and_leaves_no_wish", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "the_wake_is_the_retry_then_nothing_once_in_flight", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "a_close_while_full_is_in_flight_waits_for_its_acknowledgement", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items/retry_tests.rs", "two_refusals_keep_two_wishes", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/items_tests.rs", "a_pair_refused_session_not_found_retries_while_running", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/keyboard_screen_tests.rs", "ctrl_c_twice_within_the_window_quits_with_the_screen_open", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app/keyboard_screen_tests.rs", "ctrl_c_with_the_notice_overlay_open_still_clears_then_quits", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app_tests.rs", "ctrl_c_twice_within_the_window_quits", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/app_tests.rs", "ctrl_c_at_the_window_edge_rearms", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/event_loop/attach_tests.rs", "an_answer_at_the_deadline_is_stashed_and_lost", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/event_loop_rail_tests.rs", "the_loop_gives_the_app_the_wall_time", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/lib_motion_tests.rs", "a_waiting_card_arms_for_ten_seconds_then_stops", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/lib_motion_tests.rs", "a_waiting_card_arms_for_ten_seconds_then_stops", 2, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/paste_image_tests.rs", "event_wakes_the_worker_before_the_advance", 1, NO_PARK, "6179406086"),
    raw_advance!("crates/tui/src/view/item_tests.rs", "completed_delegate_header_freezes_the_elapsed_and_drops_the_stop", 1, NO_PARK, "6179406086"),
];

/// A raw `FakeClock::advance` or `advance_marked` call the checker proved.
struct RawAdvanceCall {
    line: usize,
    function: String,
}

/// The ban on raw fake-clock advances outside `crates/fakes`
/// (`docs/testing.md`, "Waits and timeouts"): every raw `FakeClock::advance`
/// or `advance_marked` call the checker proves needs exactly one
/// `RAW_ADVANCE_ALLOWANCES` entry keyed by path, enclosing function and
/// occurrence index, or it fails with path and line. Non-`FakeClock` methods
/// (`CaseClock`, `GateClock`, `Home`, `SourceCursor`, an in-file type's own
/// `advance`) are ignored, not allowances; no path is excluded for them.
/// Comments and string contents never count, as for `signal_sites`.
///
/// A receiver counts as `FakeClock` when the same file proves it: a function
/// parameter or `let` binding with a `FakeClock` type, a binding initialized
/// by a `FakeClock` constructor, a clone of a proved receiver
/// (`x.clone()`, `Arc::clone(&x)`, `Arc::new(x)`), a `base.field` access
/// whose owning struct declares the field with a `FakeClock` type and whose
/// base proves to be that owner (a parameter, a `let` with an owner type or
/// an `Owner::...` initializer, or `self` inside an `impl` over the owner),
/// or `self` inside an `impl` over `FakeClock`. A direct `FakeClock::...`
/// constructor result receiving the call proves too. Bindings are scoped: an
/// inner shadow is undone at its `}`. A helper call proves through its
/// declared return type: the same file's single definition first, else the
/// scanned set's single definition; a tuple return proves only the positions
/// that name `FakeClock`. Zero or conflicting definitions prove nothing, and
/// a constructor token inside a call's arguments never does, so a helper
/// that merely takes a clock stays outside the proof.
///
/// The clock's own implementation and self-tests are skipped by path below,
/// as `crates/fakes` is: they exercise the primitive itself with raw moves,
/// so the ban does not apply to them.
pub(crate) fn raw_advance_sites(files: &[RustFile]) -> Vec<String> {
    raw_advance_sites_with(files, RAW_ADVANCE_ALLOWANCES)
}

fn raw_advance_sites_with(files: &[RustFile], allowances: &[RawAdvanceAllowance]) -> Vec<String> {
    let mut remaining: Vec<&RawAdvanceAllowance> = allowances.iter().collect();
    let mut failures = Vec::new();
    let mut scanned: Vec<(&RustFile, Vec<SourceToken>)> = Vec::new();
    for file in files {
        if file.path == "crates/fakes" || file.path.starts_with("crates/fakes/") {
            continue;
        }
        // The clock implementation moved to support::clock (#3426); its own
        // files exercise the primitive itself, so the ban skips them too.
        if file.path == "crates/support/src/clock.rs"
            || file.path == "crates/support/src/clock_tests.rs"
        {
            continue;
        }
        match source_tokens(&file.source) {
            Ok(tokens) => scanned.push((file, tokens)),
            Err(stalled) => {
                failures.push(stalled_cursor_diagnostic(file, stalled));
                continue;
            }
        }
    }
    let mut helpers: BTreeMap<String, Vec<Vec<bool>>> = BTreeMap::new();
    let mut same_sets: Vec<Vec<(String, Vec<bool>)>> = Vec::new();
    for (_, tokens) in &scanned {
        let same = file_helper_returns(tokens);
        for (name, parts) in &same {
            helpers.entry(name.clone()).or_default().push(parts.clone());
        }
        same_sets.push(same);
    }
    for ((file, tokens), same) in scanned.iter().zip(same_sets.iter()) {
        let mut seen: BTreeMap<String, u32> = BTreeMap::new();
        for call in raw_advance_calls(tokens, same, &helpers) {
            let occurrence = seen
                .entry(call.function.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            let occurrence = *occurrence;
            let matched: Vec<usize> = remaining
                .iter()
                .enumerate()
                .filter(|(_, allowance)| {
                    allowance.path == file.path
                        && allowance.function == call.function
                        && allowance.occurrence == occurrence
                })
                .map(|(index, _)| index)
                .collect();
            let duplicates = matched.len();
            if duplicates == 1 {
                if let Some(index) = matched.first() {
                    let allowance = remaining.remove(*index);
                    if !allowance_pair_valid(allowance.reason, allowance.ruling) {
                        failures.push(format!(
                            "{}:{}: raw FakeClock advance allowance is invalid (function {}, occurrence {}; reason {} with ruling {} is not a documented pair)",
                            file.path,
                            call.line,
                            call.function,
                            occurrence,
                            allowance.reason,
                            allowance.ruling
                        ));
                    }
                }
            } else {
                let reasons = matched
                    .iter()
                    .filter_map(|index| remaining.get(*index))
                    .map(|allowance| format!("{} (ruling {})", allowance.reason, allowance.ruling))
                    .collect::<Vec<_>>()
                    .join(", ");
                for index in matched.into_iter().rev() {
                    remaining.remove(index);
                }
                failures.push(if duplicates == 0 {
                    format!(
                        "{}:{}: raw FakeClock advance without an allowance (function {}, occurrence {})",
                        file.path, call.line, call.function, occurrence
                    )
                } else {
                    format!(
                        "{}:{}: raw FakeClock advance matches {} allowances, want exactly one (function {}, occurrence {}; {})",
                        file.path,
                        call.line,
                        duplicates,
                        call.function,
                        occurrence,
                        reasons
                    )
                });
            }
        }
    }
    for allowance in remaining {
        failures.push(format!(
            "{} {} occurrence {} ({}; ruling {}): allowance matches no raw FakeClock advance",
            allowance.path,
            allowance.function,
            allowance.occurrence,
            allowance.reason,
            allowance.ruling
        ));
    }
    failures
}

/// Whether the tokens from `start` up to `end` name `FakeClock`.
fn range_mentions_fake_clock(tokens: &[SourceToken], start: usize, end: usize) -> bool {
    let mut i = start;
    while i < end && i < tokens.len() {
        if ident(tokens.get(i), "FakeClock") {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether the token opens a bracket group.
fn is_open(kind: Option<&SourceKind>) -> bool {
    matches!(
        kind,
        Some(SourceKind::Punct('(') | SourceKind::Punct('[') | SourceKind::Punct('{'))
    )
}

/// Whether the token closes a bracket group.
fn is_close(kind: Option<&SourceKind>) -> bool {
    matches!(
        kind,
        Some(SourceKind::Punct(')') | SourceKind::Punct(']') | SourceKind::Punct('}'))
    )
}

/// The last identifier before the `:` at `colon`, if any.
fn last_field_name(tokens: &[SourceToken], start: usize, colon: usize) -> Option<String> {
    let mut name = None;
    let mut s = start;
    while s < colon {
        if let Some(word) = tokens.get(s).and_then(|token| match &token.kind {
            SourceKind::Ident(word) => Some(word.clone()),
            SourceKind::String(_) | SourceKind::Punct(_) => None,
        }) {
            name = Some(word);
        }
        s += 1;
    }
    name
}

/// Every `struct` the file declares, owners and non-owners alike, so a
/// proved base resolves to its declared owner and only an (`owner`, `field`)
/// pair with a `FakeClock` field proves; a proved non-owner base
/// (`g: G` where `G.clock` is not a `FakeClock`) and an unproved base both
/// prove nothing (docs/testing.md:254-257).
fn in_file_structs(tokens: &[SourceToken]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut i = 0;
    while i < tokens.len() {
        if ident(tokens.get(i), "struct") {
            let mut j = i + 1;
            while j < tokens.len()
                && !punct(tokens.get(j), '{')
                && !punct(tokens.get(j), ';')
                && !punct(tokens.get(j), '(')
            {
                if let Some(word) = tokens.get(j).and_then(|token| match &token.kind {
                    SourceKind::Ident(word) => Some(word.clone()),
                    SourceKind::String(_) | SourceKind::Punct(_) => None,
                }) {
                    names.insert(word);
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    names
}

/// Whether the `base.field` access proves a `FakeClock`: `field` belongs to
/// `owner` and `base` proves to be that `owner` (`self` through the enclosing
/// `impl`, any other base through its tracked struct type). A base whose
/// type the file does not prove proves nothing, so a same-named field of a
/// helper return or a cross-file type is never reported
/// (docs/testing.md:254-257).
fn field_proves_fake(
    fields: &BTreeSet<(String, String)>,
    base: &str,
    field: &str,
    structs: &BTreeMap<String, String>,
    impl_name: Option<&str>,
) -> bool {
    if base == "self" {
        let Some(owner) = impl_name else {
            return false;
        };
        return fields.contains(&(owner.to_owned(), field.to_owned()));
    }
    let Some(owner) = structs.get(base) else {
        return false;
    };
    fields.contains(&(owner.clone(), field.to_owned()))
}

/// The first in-file struct name mentioned from `start` up to `end`, if any.
fn type_mentions_owner(
    tokens: &[SourceToken],
    start: usize,
    end: usize,
    known: &BTreeSet<String>,
) -> Option<String> {
    let mut i = start;
    while i < end && i < tokens.len() {
        if let Some(word) = tokens.get(i).and_then(|token| match &token.kind {
            SourceKind::Ident(word) => Some(word.as_str()),
            SourceKind::String(_) | SourceKind::Punct(_) => None,
        }) && known.contains(word)
        {
            return Some(word.to_owned());
        }
        i += 1;
    }
    None
}

/// The owning struct a `let` initializer proves for its binding: `Owner::...`
/// or `Owner { ... }`, or `Self::...` inside an `impl` over an in-file struct.
fn initializer_owner(
    tokens: &[SourceToken],
    start: usize,
    end: usize,
    known: &BTreeSet<String>,
    impl_name: Option<&str>,
) -> Option<String> {
    let mut i = start;
    while i < end && i < tokens.len() {
        match tokens.get(i).map(|token| &token.kind) {
            Some(SourceKind::Ident(word)) => {
                if word == "Self" {
                    return impl_name
                        .filter(|name| known.contains(*name))
                        .map(str::to_owned);
                }
                if known.contains(word.as_str()) {
                    return Some(word.clone());
                }
                return None;
            }
            Some(SourceKind::Punct('&' | '*' | '(')) => {
                i += 1;
                continue;
            }
            Some(SourceKind::String(_)) | Some(SourceKind::Punct(_)) => {
                return None;
            }
            None => return None,
        }
    }
    None
}

/// The `impl` self type at `tokens[at]`: the type after `for` when present,
/// else the type after `impl`, skipping generics (docs/testing.md:254-257).
fn impl_self_name(tokens: &[SourceToken], at: usize) -> Option<String> {
    let mut brace = None;
    let mut j = at + 1;
    while j < tokens.len() {
        if punct(tokens.get(j), '{') {
            brace = Some(j);
            break;
        }
        j += 1;
    }
    let brace = brace?;
    let mut angle: usize = 0;
    let mut for_pos = None;
    let mut k = at + 1;
    while k < brace {
        if punct(tokens.get(k), '<') {
            angle += 1;
        } else if punct(tokens.get(k), '>') {
            angle = angle.saturating_sub(1);
        } else if angle == 0 && ident(tokens.get(k), "for") {
            for_pos = Some(k);
            break;
        }
        k += 1;
    }
    let mut m = for_pos.map_or(at + 1, |pos| pos + 1);
    let mut inner: usize = 0;
    while m < brace {
        if punct(tokens.get(m), '<') {
            inner += 1;
        } else if punct(tokens.get(m), '>') {
            inner = inner.saturating_sub(1);
        } else if inner == 0
            && let Some(word) = tokens.get(m).and_then(|token| match &token.kind {
                SourceKind::Ident(word) => Some(word.as_str()),
                SourceKind::String(_) | SourceKind::Punct(_) => None,
            })
            && word.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            return Some(word.to_owned());
        }
        m += 1;
    }
    None
}

/// Whether the field type after `colon` names `FakeClock` before its
/// closing comma or brace.
fn type_mentions_fake_clock(tokens: &[SourceToken], mut m: usize) -> bool {
    let mut inner: usize = 0;
    while m < tokens.len() {
        let kind = tokens.get(m).map(|token| &token.kind);
        if is_open(kind) {
            inner += 1;
        } else if is_close(kind) {
            break;
        } else if inner == 0 {
            if matches!(kind, Some(SourceKind::Punct(','))) {
                break;
            }
            if matches!(kind, Some(SourceKind::Ident(word)) if word.as_str() == "FakeClock") {
                return true;
            }
        }
        m += 1;
    }
    false
}

/// (`owner`, `field`) pairs an in-file `struct` declares with a `FakeClock`
/// type, so a `base.field` receiver with a proved `base` proves only for its
/// `owner`; an unproved `base` proves nothing
/// (docs/testing.md:254-257).
fn fake_clock_fields(tokens: &[SourceToken]) -> BTreeSet<(String, String)> {
    let mut fields = BTreeSet::new();
    let mut i = 0;
    while i < tokens.len() {
        if ident(tokens.get(i), "struct") {
            let mut owner = None;
            let mut s = i + 1;
            while s < tokens.len()
                && !punct(tokens.get(s), '{')
                && !punct(tokens.get(s), ';')
                && !punct(tokens.get(s), '(')
            {
                if let Some(word) = tokens.get(s).and_then(|token| match &token.kind {
                    SourceKind::Ident(word) => Some(word.clone()),
                    SourceKind::String(_) | SourceKind::Punct(_) => None,
                }) && owner.is_none()
                {
                    owner = Some(word);
                }
                s += 1;
            }
            let mut j = i + 1;
            while j < tokens.len()
                && !punct(tokens.get(j), '{')
                && !punct(tokens.get(j), ';')
                && !punct(tokens.get(j), '(')
            {
                j += 1;
            }
            if punct(tokens.get(j), '{') {
                let mut depth: usize = 1;
                let mut k = j + 1;
                let mut segment_start = k;
                while k < tokens.len() && depth > 0 {
                    let kind = tokens.get(k).map(|token| &token.kind);
                    if is_open(kind) {
                        depth += 1;
                    } else if is_close(kind) {
                        depth = depth.saturating_sub(1);
                    }
                    if depth == 1
                        && punct(tokens.get(k), ':')
                        && let Some(name) = last_field_name(tokens, segment_start, k)
                        && name != "pub"
                        && type_mentions_fake_clock(tokens, k + 1)
                        && let Some(owner) = owner.clone()
                    {
                        fields.insert((owner, name));
                    }
                    if depth == 1 && punct(tokens.get(k), ',') {
                        segment_start = k + 1;
                    }
                    k += 1;
                }
                i = k;
            }
        }
        i += 1;
    }
    fields
}

/// The receiver chain before the `.` at `dot`, base first: idents joined by
/// dots, with `[...]` index groups skipped.
fn receiver_chain(tokens: &[SourceToken], dot: usize) -> Vec<String> {
    let mut chain = Vec::new();
    if dot == 0 {
        return chain;
    }
    let mut j = dot - 1;
    loop {
        match tokens.get(j).map(|token| &token.kind) {
            Some(SourceKind::Ident(name)) => {
                chain.push(name.clone());
                let Some(before) = j.checked_sub(1).and_then(|at| tokens.get(at)) else {
                    break;
                };
                if matches!(before.kind, SourceKind::Punct('.')) {
                    let Some(prev) = j.checked_sub(2) else {
                        break;
                    };
                    j = prev;
                } else {
                    break;
                }
            }
            Some(SourceKind::Punct(']')) => {
                let mut inner: usize = 1;
                let Some(mut at) = j.checked_sub(1) else {
                    break;
                };
                while inner > 0 {
                    match tokens.get(at).map(|token| &token.kind) {
                        Some(SourceKind::Punct(']')) => inner += 1,
                        Some(SourceKind::Punct('[')) => {
                            inner = inner.saturating_sub(1);
                        }
                        Some(SourceKind::Ident(_))
                        | Some(SourceKind::String(_))
                        | Some(SourceKind::Punct(_)) => {}
                        None => break,
                    }
                    if inner == 0 {
                        break;
                    }
                    let Some(prev) = at.checked_sub(1) else {
                        break;
                    };
                    at = prev;
                }
                j = at;
                let Some(before) = j.checked_sub(1).and_then(|at| tokens.get(at)) else {
                    break;
                };
                if matches!(before.kind, SourceKind::Punct('.')) {
                    let Some(prev) = j.checked_sub(2) else {
                        break;
                    };
                    j = prev;
                } else {
                    break;
                }
            }
            Some(SourceKind::String(_)) | Some(SourceKind::Punct(_)) | None => break,
        }
    }
    chain.reverse();
    chain
}

struct AdvanceFnFrame {
    name: String,
    depth: usize,
    fake: BTreeSet<String>,
    fake_undos: Vec<(usize, String, bool)>,
    structs: BTreeMap<String, String>,
    struct_undos: Vec<(usize, String, Option<String>)>,
}
/// The parameter from `start` up to `end`: its name when its type names
/// `FakeClock`, `self` included only with its own `FakeClock` type.
fn param_proves_fake(tokens: &[SourceToken], start: usize, end: usize) -> Option<String> {
    let mut words = Vec::new();
    let mut i = start;
    while i < end {
        if let Some(word) = tokens.get(i).and_then(|token| match &token.kind {
            SourceKind::Ident(word) => Some(word.as_str()),
            SourceKind::String(_) | SourceKind::Punct(_) => None,
        }) {
            words.push(word);
        }
        i += 1;
    }
    if words.first().is_some_and(|word| *word == "mut") {
        words.remove(0);
    }
    let first = words.first().copied()?;
    let mut colon = false;
    let mut c = start;
    while c < end {
        if matches!(
            tokens.get(c).map(|token| &token.kind),
            Some(SourceKind::Punct(':'))
        ) {
            colon = true;
            break;
        }
        c += 1;
    }
    if !colon || !range_mentions_fake_clock(tokens, start, end) {
        return None;
    }
    if first == "self" {
        return Some("self".to_owned());
    }
    Some(first.to_owned())
}

/// The `base` variable and owning struct for the parameter from `start` up
/// to `end`, when its type names an owner of a `FakeClock` field.
fn param_struct_owner(
    tokens: &[SourceToken],
    start: usize,
    end: usize,
    known: &BTreeSet<String>,
) -> Option<(String, String)> {
    let mut colon = None;
    let mut c = start;
    while c < end {
        if punct(tokens.get(c), ':') {
            colon = Some(c);
            break;
        }
        c += 1;
    }
    let colon = colon?;
    let mut words = Vec::new();
    let mut i = start;
    while i < colon {
        if let Some(word) = tokens.get(i).and_then(|token| match &token.kind {
            SourceKind::Ident(word) => Some(word.as_str()),
            SourceKind::String(_) | SourceKind::Punct(_) => None,
        }) {
            words.push(word);
        }
        i += 1;
    }
    if words.first().is_some_and(|word| *word == "mut") {
        words.remove(0);
    }
    let first = (*words.first()?).to_owned();
    if first == "self" {
        return None;
    }
    let owner = type_mentions_owner(tokens, colon + 1, end, known)?;
    Some((first, owner))
}

/// The `fn` signature at `tokens[at]`: its name, the parameters with a
/// `FakeClock` type (`self` included when its own type names one), and the
/// parameters whose type names an in-file struct.
fn fn_signature(
    tokens: &[SourceToken],
    at: usize,
    known: &BTreeSet<String>,
) -> Option<(String, BTreeSet<String>, BTreeMap<String, String>)> {
    let name = tokens.get(at + 1).and_then(|token| match &token.kind {
        SourceKind::Ident(name) => Some(name.clone()),
        SourceKind::String(_) | SourceKind::Punct(_) => None,
    })?;
    let mut open = None;
    let mut cursor = at + 2;
    while cursor < tokens.len() {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if matches!(kind, Some(SourceKind::Punct('('))) {
            open = Some(cursor);
            break;
        }
        if matches!(kind, Some(SourceKind::Punct('{') | SourceKind::Punct(';'))) {
            return None;
        }
        cursor += 1;
    }
    let open = open?;
    let mut depth: usize = 1;
    let mut close = None;
    cursor = open + 1;
    while cursor < tokens.len() {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if is_open(kind) {
            depth += 1;
        } else if is_close(kind) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                close = Some(cursor);
                break;
            }
        }
        cursor += 1;
    }
    let close = close?;
    let mut fake = BTreeSet::new();
    let mut structs = BTreeMap::new();
    let mut start = open + 1;
    let mut inner: usize = 0;
    let mut end = open + 1;
    while end <= close {
        let kind = tokens.get(end).map(|token| &token.kind);
        let is_edge = end == close || (inner == 0 && matches!(kind, Some(SourceKind::Punct(','))));
        if is_open(kind) {
            inner += 1;
        } else if is_close(kind) {
            inner = inner.saturating_sub(1);
        }
        if is_edge {
            if let Some(param) = param_proves_fake(tokens, start, end) {
                fake.insert(param);
            }
            if let Some((var, owner)) = param_struct_owner(tokens, start, end, known) {
                structs.insert(var, owner);
            }
            start = end + 1;
        }
        end += 1;
    }
    Some((name, fake, structs))
}

/// The depth-zero comma-separated spans of `start..end`, so a tuple pattern
/// and a tuple return type split at the same positions.
fn split_top_level(tokens: &[SourceToken], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut parts = Vec::new();
    let mut depth: usize = 0;
    let mut part_start = start;
    let mut cursor = start;
    while cursor < end {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if is_open(kind) {
            depth += 1;
        } else if is_close(kind) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && matches!(kind, Some(SourceKind::Punct(','))) {
            parts.push((part_start, cursor));
            part_start = cursor + 1;
        }
        cursor += 1;
    }
    if part_start < end {
        parts.push((part_start, end));
    }
    parts
}

/// A helper's return type as one `FakeClock` flag per top-level tuple part,
/// or a single flag for a non-tuple return; a missing return type proves
/// nothing.
fn helper_return_parts(tokens: &[SourceToken], at: usize) -> Option<(String, Vec<bool>)> {
    let name = tokens.get(at + 1).and_then(|token| match &token.kind {
        SourceKind::Ident(name) => Some(name.clone()),
        SourceKind::String(_) | SourceKind::Punct(_) => None,
    })?;
    let mut open = None;
    let mut cursor = at + 2;
    while cursor < tokens.len() {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if matches!(kind, Some(SourceKind::Punct('('))) {
            open = Some(cursor);
            break;
        }
        if matches!(kind, Some(SourceKind::Punct('{') | SourceKind::Punct(';'))) {
            return None;
        }
        cursor += 1;
    }
    let open = open?;
    let mut depth: usize = 1;
    let mut close = None;
    cursor = open + 1;
    while cursor < tokens.len() {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if is_open(kind) {
            depth += 1;
        } else if is_close(kind) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                close = Some(cursor);
                break;
            }
        }
        cursor += 1;
    }
    let close = close?;
    cursor = close + 1;
    if !(punct(tokens.get(cursor), '-') && punct(tokens.get(cursor + 1), '>')) {
        return Some((name, vec![false]));
    }
    let start = cursor + 2;
    let mut end = start;
    let mut inner: usize = 0;
    while end < tokens.len() {
        let kind = tokens.get(end).map(|token| &token.kind);
        // A return type never holds a brace: the body's `{` always ends it,
        // so a later constructor token cannot leak into the type.
        if matches!(kind, Some(SourceKind::Punct('{'))) {
            break;
        }
        if is_open(kind) {
            inner += 1;
        } else if is_close(kind) {
            inner = inner.saturating_sub(1);
        } else if inner == 0 && matches!(kind, Some(SourceKind::Punct(';'))) {
            break;
        }
        end += 1;
    }
    if end <= start {
        return Some((name, vec![false]));
    }
    if punct(tokens.get(start), '(') {
        let mut nested: usize = 1;
        cursor = start + 1;
        while cursor < end {
            let kind = tokens.get(cursor).map(|token| &token.kind);
            if is_open(kind) {
                nested += 1;
            } else if is_close(kind) {
                nested = nested.saturating_sub(1);
                if nested == 0 {
                    break;
                }
            }
            cursor += 1;
        }
        if cursor == end - 1 {
            let mut parts = Vec::new();
            for (part_start, part_end) in split_top_level(tokens, start + 1, cursor) {
                parts.push(range_mentions_fake_clock(tokens, part_start, part_end));
            }
            if parts.is_empty() {
                parts.push(false);
            }
            return Some((name, parts));
        }
    }
    Some((name, vec![range_mentions_fake_clock(tokens, start, end)]))
}

/// Every `fn` name in the file with its return type's `FakeClock` positions,
/// so a call proves only what its callee declares.
fn file_helper_returns(tokens: &[SourceToken]) -> Vec<(String, Vec<bool>)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if ident(tokens.get(i), "fn")
            && let Some(entry) = helper_return_parts(tokens, i)
        {
            found.push(entry);
        }
        i += 1;
    }
    found
}

/// The return type's `FakeClock` positions for a call to `name`: the same
/// file's single definition first, else the scanned set's single definition.
/// Zero or conflicting definitions prove nothing, so a shared helper name
/// never guesses across files.
fn helper_proof(
    name: &str,
    same: &[(String, Vec<bool>)],
    helpers: &BTreeMap<String, Vec<Vec<bool>>>,
) -> Option<Vec<bool>> {
    let mut local = same
        .iter()
        .filter(|(defined, _)| defined == name)
        .map(|(_, parts)| parts);
    let first = local.next();
    if local.next().is_some() {
        return None;
    }
    if let Some(parts) = first {
        return Some((*parts).clone());
    }
    match helpers.get(name) {
        Some(all) if all.len() == 1 => all.first().cloned(),
        _ => None,
    }
}

/// Whether the receiver atom from `start` up to `end` is proved: a single
/// proved binding, `self` with a proved receiver, or a `base.field` access
/// whose owning struct declares the field with a `FakeClock` type and whose
/// base proves to be that owner.
#[allow(
    clippy::too_many_arguments,
    reason = "one span plus one proof input per source"
)]
fn atom_proves_fake(
    tokens: &[SourceToken],
    start: usize,
    end: usize,
    fake: &BTreeSet<String>,
    fake_fields: &BTreeSet<(String, String)>,
    structs: &BTreeMap<String, String>,
    impl_name: Option<&str>,
    self_fake: bool,
) -> bool {
    let mut cursor = start;
    while cursor < end {
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if matches!(kind, Some(SourceKind::Punct('&'))) {
            cursor += 1;
            continue;
        }
        if matches!(kind, Some(SourceKind::Ident(word)) if word.as_str() == "mut") {
            cursor += 1;
            continue;
        }
        break;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    while cursor < end {
        match tokens.get(cursor).map(|token| &token.kind) {
            Some(SourceKind::Ident(word)) => current.push_str(word.as_str()),
            Some(SourceKind::Punct('.')) => {
                parts.push(std::mem::take(&mut current));
            }
            Some(SourceKind::Punct('[')) => {
                let mut inner: usize = 1;
                cursor += 1;
                while cursor < end && inner > 0 {
                    let kind = tokens.get(cursor).map(|token| &token.kind);
                    if matches!(kind, Some(SourceKind::Punct('['))) {
                        inner += 1;
                    } else if matches!(kind, Some(SourceKind::Punct(']'))) {
                        inner = inner.saturating_sub(1);
                    }
                    cursor += 1;
                }
                continue;
            }
            Some(SourceKind::String(_)) | Some(SourceKind::Punct(_)) | None => break,
        }
        cursor += 1;
    }
    parts.push(current);
    parts.retain(|part| !part.is_empty());
    if parts.len() == 1 {
        return parts.first().is_some_and(|only| {
            fake.contains(only.as_str()) || (only.as_str() == "self" && self_fake)
        });
    }
    if parts.len() == 2 {
        let (Some(base), Some(field)) = (parts.first(), parts.last()) else {
            return false;
        };
        return field_proves_fake(fake_fields, base, field, structs, impl_name);
    }
    false
}

/// The outermost `::`-separated callee path at `start` with its opening
/// paren, such as `FakeClock::new()` or `wrap(...)`, or None when the span
/// does not open with a call. The path never reaches into arguments, so a
/// constructor token inside them proves nothing.
fn outer_call_path(tokens: &[SourceToken], start: usize) -> Option<(Vec<String>, usize)> {
    let mut path = Vec::new();
    let mut cursor = start;
    loop {
        match tokens.get(cursor).map(|token| &token.kind) {
            Some(SourceKind::Ident(word)) => {
                path.push(word.clone());
                cursor += 1;
            }
            _ => return None,
        }
        if punct(tokens.get(cursor), ':') && punct(tokens.get(cursor + 1), ':') {
            cursor += 2;
            continue;
        }
        break;
    }
    if path.is_empty() || !punct(tokens.get(cursor), '(') {
        return None;
    }
    Some((path, cursor))
}

/// The binding a tuple-pattern part declares when it is a plain name, such
/// as `clock` or `mut clock`; anything else (a rest pattern, a nested
/// pattern) binds nothing here.
fn simple_pattern_name(tokens: &[SourceToken], start: usize, end: usize) -> Option<String> {
    let mut cursor = start;
    if ident(tokens.get(cursor), "mut") {
        cursor += 1;
    }
    match tokens.get(cursor).map(|token| &token.kind) {
        Some(SourceKind::Ident(binding)) if cursor + 1 == end => Some(binding.clone()),
        _ => None,
    }
}

/// Whether the call result receiving `.advance` at `dot` proves a
/// `FakeClock`: a `FakeClock` constructor result, or a plain helper call
/// whose single declared return names one. A method result and a qualified
/// non-`FakeClock` call prove nothing.
#[allow(
    clippy::too_many_arguments,
    reason = "one span plus one proof input per source"
)]
fn call_result_proves_fake(
    tokens: &[SourceToken],
    dot: usize,
    same: &[(String, Vec<bool>)],
    helpers: &BTreeMap<String, Vec<Vec<bool>>>,
) -> bool {
    if dot == 0 || !punct(tokens.get(dot - 1), ')') {
        return false;
    }
    let mut depth: usize = 0;
    let mut cursor = dot;
    while cursor > 0 {
        cursor -= 1;
        let kind = tokens.get(cursor).map(|token| &token.kind);
        if matches!(kind, Some(SourceKind::Punct(')'))) {
            depth += 1;
        } else if matches!(kind, Some(SourceKind::Punct('('))) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                break;
            }
        }
    }
    if depth != 0 {
        return false;
    }
    let mut path = Vec::new();
    let mut back = cursor;
    while let Some(previous) = back.checked_sub(1).and_then(|at| tokens.get(at)) {
        match &previous.kind {
            SourceKind::Ident(word) => {
                path.push(word.clone());
                back -= 1;
            }
            SourceKind::String(_) | SourceKind::Punct(_) => break,
        }
        if back >= 2 && punct(tokens.get(back - 1), ':') && punct(tokens.get(back - 2), ':') {
            back -= 2;
            continue;
        }
        break;
    }
    if path.is_empty() {
        return false;
    }
    path.reverse();
    if back > 0 && punct(tokens.get(back - 1), '.') {
        return false;
    }
    if path.iter().any(|segment| segment == "FakeClock") {
        return true;
    }
    if let [name] = path.as_slice() {
        return helper_proof(name, same, helpers)
            .is_some_and(|parts| matches!(parts.as_slice(), [true]));
    }
    false
}

/// Whether the initializer from `start` up to `end` proves a `FakeClock`:
/// a `FakeClock` constructor in the outermost callee, a clone of an already
/// proved receiver, or a plain helper call whose single declared return
/// names one.
#[allow(
    clippy::too_many_arguments,
    reason = "one span plus one proof input per source"
)]
fn init_proves_fake(
    tokens: &[SourceToken],
    start: usize,
    end: usize,
    fake: &BTreeSet<String>,
    fake_fields: &BTreeSet<(String, String)>,
    structs: &BTreeMap<String, String>,
    impl_name: Option<&str>,
    self_fake: bool,
    same: &[(String, Vec<bool>)],
    helpers: &BTreeMap<String, Vec<Vec<bool>>>,
) -> bool {
    // `x.clone()`
    if end.saturating_sub(start) >= 5
        && matches!(
            tokens.get(start).map(|token| &token.kind),
            Some(SourceKind::Ident(_))
        )
        && punct(tokens.get(start + 1), '.')
        && ident(tokens.get(start + 2), "clone")
        && punct(tokens.get(start + 3), '(')
        && punct(tokens.get(start + 4), ')')
    {
        return atom_proves_fake(
            tokens,
            start,
            start + 1,
            fake,
            fake_fields,
            structs,
            impl_name,
            self_fake,
        );
    }
    let Some((path, open)) = outer_call_path(tokens, start) else {
        return false;
    };
    // A `FakeClock` constructor in the outermost callee, however qualified.
    if path.iter().any(|segment| segment == "FakeClock") {
        return true;
    }
    // `Arc::clone(&x)`, `Arc::clone(x)`, `Arc::new(x)`, `Box::new(x)` and
    // `Box::leak(x)`: standard-library wrappers transparent to their
    // argument's type, so the inner value proves as it would unwrapped.
    if path.len() == 2
        && ((path.first().is_some_and(|first| first == "Arc")
            && path
                .last()
                .is_some_and(|last| last == "clone" || last == "new"))
            || (path.first().is_some_and(|first| first == "Box")
                && path
                    .last()
                    .is_some_and(|last| last == "new" || last == "leak")))
    {
        let mut inner: usize = 1;
        let mut close = end;
        let mut cursor = open + 1;
        while cursor < end {
            let kind = tokens.get(cursor).map(|token| &token.kind);
            if is_open(kind) {
                inner += 1;
            } else if is_close(kind) {
                inner = inner.saturating_sub(1);
                if inner == 0 {
                    close = cursor;
                    break;
                }
            }
            cursor += 1;
        }
        return atom_proves_fake(
            tokens,
            open + 1,
            close,
            fake,
            fake_fields,
            structs,
            impl_name,
            self_fake,
        ) || init_proves_fake(
            tokens,
            open + 1,
            close,
            fake,
            fake_fields,
            structs,
            impl_name,
            self_fake,
            same,
            helpers,
        );
    }
    // A plain helper call proves its single `FakeClock` return, if any.
    if let [name] = path.as_slice() {
        return helper_proof(name, same, helpers)
            .is_some_and(|parts| matches!(parts.as_slice(), [true]));
    }
    false
}
/// Every `.advance(` or `.advance_marked(` call the scan proves is a
/// `FakeClock` receiver, in source order: bindings the same file proves,
/// calls the scanned helpers type, and direct `FakeClock` constructor
/// results. Bindings are scoped: an inner `let` shadow is undone at its
/// closing `}` so the outer proof survives.
fn raw_advance_calls(
    tokens: &[SourceToken],
    same: &[(String, Vec<bool>)],
    helpers: &BTreeMap<String, Vec<Vec<bool>>>,
) -> Vec<RawAdvanceCall> {
    let fake_fields = fake_clock_fields(tokens);
    let all_structs = in_file_structs(tokens);
    let mut calls = Vec::new();
    let mut depth: usize = 0;
    let mut pending_impl: Option<(bool, Option<String>)> = None;
    let mut pending_fn: Option<(String, BTreeSet<String>, BTreeMap<String, String>)> = None;
    let mut impl_frames: Vec<(usize, bool, Option<String>)> = Vec::new();
    let mut fn_frames: Vec<AdvanceFnFrame> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if ident(tokens.get(i), "impl") {
            let mut header_fake = false;
            let mut j = i + 1;
            while j < tokens.len() && !punct(tokens.get(j), '{') {
                if ident(tokens.get(j), "FakeClock") {
                    header_fake = true;
                }
                j += 1;
            }
            pending_impl = Some((header_fake, impl_self_name(tokens, i)));
        } else if ident(tokens.get(i), "fn") {
            if let Some((name, fake, structs)) = fn_signature(tokens, i, &all_structs) {
                pending_fn = Some((name, fake, structs));
            }
        } else if ident(tokens.get(i), "let") && !fn_frames.is_empty() {
            let (frame_fake, frame_structs, frame_impl_name, frame_self_fake) =
                fn_frames.last().map_or_else(
                    || (BTreeSet::new(), BTreeMap::new(), None, false),
                    |frame| {
                        let self_fake = impl_frames.last().is_some_and(|(_, fake, _)| *fake)
                            || frame.fake.contains("self");
                        (
                            frame.fake.clone(),
                            frame.structs.clone(),
                            impl_frames.last().and_then(|(_, _, name)| name.clone()),
                            self_fake,
                        )
                    },
                );
            let impl_name = frame_impl_name.as_deref();
            let Some(frame) = fn_frames.last_mut() else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            if ident(tokens.get(j), "mut") {
                j += 1;
            }
            if punct(tokens.get(j), '(') {
                let mut paren: usize = 1;
                let mut k = j + 1;
                while k < tokens.len() {
                    let kind = tokens.get(k).map(|token| &token.kind);
                    if is_open(kind) {
                        paren += 1;
                    } else if is_close(kind) {
                        paren = paren.saturating_sub(1);
                        if paren == 0 {
                            break;
                        }
                    }
                    k += 1;
                }
                if paren != 0 || !punct(tokens.get(k + 1), '=') {
                    i += 1;
                    continue;
                }
                let mut m = k + 2;
                let mut inner: usize = 0;
                while m < tokens.len() {
                    let kind = tokens.get(m).map(|token| &token.kind);
                    if is_open(kind) {
                        inner += 1;
                    } else if is_close(kind) {
                        inner = inner.saturating_sub(1);
                    } else if inner == 0 && matches!(kind, Some(SourceKind::Punct(';'))) {
                        break;
                    }
                    m += 1;
                }
                let proof = match tokens.get(k + 2).map(|token| &token.kind) {
                    Some(SourceKind::Ident(callee)) if punct(tokens.get(k + 3), '(') => {
                        helper_proof(callee, same, helpers)
                    }
                    _ => None,
                };
                for (index, (part_start, part_end)) in
                    split_top_level(tokens, j + 1, k).iter().enumerate()
                {
                    let Some(binding) = simple_pattern_name(tokens, *part_start, *part_end) else {
                        continue;
                    };
                    let proved = proof
                        .as_ref()
                        .is_some_and(|positions| positions.get(index) == Some(&true));
                    frame
                        .fake_undos
                        .push((depth, binding.clone(), frame.fake.contains(&binding)));
                    if proved {
                        frame.fake.insert(binding.clone());
                    } else {
                        frame.fake.remove(&binding);
                    }
                    frame.struct_undos.push((
                        depth,
                        binding.clone(),
                        frame.structs.get(&binding).cloned(),
                    ));
                    frame.structs.remove(&binding);
                }
            } else if let Some(name) = tokens.get(j).and_then(|token| match &token.kind {
                SourceKind::Ident(name) => Some(name.clone()),
                SourceKind::String(_) | SourceKind::Punct(_) => None,
            }) {
                let mut k = j + 1;
                let mut typed_fake = false;
                let mut type_owner: Option<String> = None;
                if punct(tokens.get(k), ':') {
                    let mut m = k + 1;
                    let mut inner: usize = 0;
                    while m < tokens.len() {
                        let kind = tokens.get(m).map(|token| &token.kind);
                        if is_open(kind) {
                            inner += 1;
                        } else if is_close(kind) {
                            inner = inner.saturating_sub(1);
                        } else if inner == 0 {
                            if matches!(kind, Some(SourceKind::Punct('=' | ';'))) {
                                break;
                            }
                            if matches!(kind, Some(SourceKind::Ident(word)) if word.as_str() == "FakeClock")
                            {
                                typed_fake = true;
                            }
                        }
                        m += 1;
                    }
                    type_owner = type_mentions_owner(tokens, k + 1, m, &all_structs);
                    k = m;
                }
                if punct(tokens.get(k), '=') {
                    let mut m = k + 1;
                    let mut inner: usize = 0;
                    while m < tokens.len() {
                        let kind = tokens.get(m).map(|token| &token.kind);
                        if is_open(kind) {
                            inner += 1;
                        } else if is_close(kind) {
                            inner = inner.saturating_sub(1);
                        } else if inner == 0 && matches!(kind, Some(SourceKind::Punct(';'))) {
                            break;
                        }
                        m += 1;
                    }
                    let proved = typed_fake
                        || init_proves_fake(
                            tokens,
                            k + 1,
                            m,
                            &frame_fake,
                            &fake_fields,
                            &frame_structs,
                            impl_name,
                            frame_self_fake,
                            same,
                            helpers,
                        );
                    let init_owner = initializer_owner(tokens, k + 1, m, &all_structs, impl_name);
                    let owner = type_owner.or(init_owner);
                    frame
                        .fake_undos
                        .push((depth, name.clone(), frame.fake.contains(&name)));
                    if proved {
                        frame.fake.insert(name.clone());
                    } else {
                        frame.fake.remove(&name);
                    }
                    frame.struct_undos.push((
                        depth,
                        name.clone(),
                        frame.structs.get(&name).cloned(),
                    ));
                    if let Some(owner) = owner {
                        frame.structs.insert(name, owner);
                    } else {
                        frame.structs.remove(&name);
                    }
                }
            }
        } else if punct(tokens.get(i), '.')
            && tokens.get(i + 1).is_some_and(|token| {
                matches!(
                    &token.kind,
                    SourceKind::Ident(name) if name == "advance" || name == "advance_marked"
                )
            })
            && punct(tokens.get(i + 2), '(')
        {
            let chain = receiver_chain(tokens, i);
            let (self_fake, impl_name) = impl_frames
                .last()
                .map_or((false, None), |(_, fake, name)| (*fake, name.as_deref()));
            let proved = match chain.as_slice() {
                [] => call_result_proves_fake(tokens, i, same, helpers),
                [base] if base.as_str() == "self" => {
                    self_fake
                        || fn_frames
                            .last()
                            .is_some_and(|frame| frame.fake.contains("self"))
                }
                [base] => fn_frames
                    .last()
                    .is_some_and(|frame| frame.fake.contains(base.as_str())),
                [base, field] => fn_frames.last().is_some_and(|frame| {
                    field_proves_fake(&fake_fields, base, field, &frame.structs, impl_name)
                }),
                [_, _, ..] => false,
            };
            if proved && let Some(dot) = tokens.get(i) {
                calls.push(RawAdvanceCall {
                    line: dot.line,
                    function: fn_frames
                        .last()
                        .map_or_else(String::new, |frame| frame.name.clone()),
                });
            }
        } else if punct(tokens.get(i), '{') {
            depth += 1;
            if let Some((fake, name)) = pending_impl.take() {
                impl_frames.push((depth, fake, name));
            }
            if let Some((name, fake, structs)) = pending_fn.take() {
                fn_frames.push(AdvanceFnFrame {
                    name,
                    depth,
                    fake,
                    fake_undos: Vec::new(),
                    structs,
                    struct_undos: Vec::new(),
                });
            }
        } else if punct(tokens.get(i), '}') {
            depth = depth.saturating_sub(1);
            impl_frames.retain(|(frame_depth, _, _)| *frame_depth <= depth);
            for frame in fn_frames.iter_mut() {
                while frame
                    .fake_undos
                    .last()
                    .is_some_and(|(undo_depth, _, _)| *undo_depth > depth)
                {
                    if let Some((_, var, was_present)) = frame.fake_undos.pop() {
                        if was_present {
                            frame.fake.insert(var);
                        } else {
                            frame.fake.remove(&var);
                        }
                    }
                }
                while frame
                    .struct_undos
                    .last()
                    .is_some_and(|(undo_depth, _, _)| *undo_depth > depth)
                {
                    if let Some((_, var, was_owner)) = frame.struct_undos.pop() {
                        if let Some(owner) = was_owner {
                            frame.structs.insert(var, owner);
                        } else {
                            frame.structs.remove(&var);
                        }
                    }
                }
            }
            fn_frames.retain(|frame| frame.depth <= depth);
        } else if punct(tokens.get(i), ';') {
            pending_fn = None;
        }
        i += 1;
    }
    calls
}

/// Whether `tree` is the `"kill"` argument: the command's name on its
/// own, not a longer string that merely mentions it.
fn is_kill_literal(tree: Option<&TokenTree>) -> bool {
    match tree {
        Some(TokenTree::Group(group)) => group
            .stream()
            .into_iter()
            .any(|tree| matches!(&tree, TokenTree::Literal(literal) if literal.to_string() == "\"kill\"")),
        _ => false,
    }
}

/// Crate names in the tables of "Runtime dependencies" and "Tests and
/// development tools".
pub(crate) fn admitted(dependencies: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for heading in ["Runtime dependencies", "Tests and development tools"] {
        let lines = section(dependencies, heading)
            .ok_or(format!("docs/dependencies.md has no \"{heading}\" section"))?;
        for cells in table_rows(&lines) {
            let first = cells.first().map_or("", String::as_str);
            names.extend(first.split(',').map(unticked_trimmed).filter(|w| {
                !w.is_empty()
                    && w.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            }));
        }
    }
    Ok(names)
}

fn unticked_trimmed(part: &str) -> String {
    unticked(part.trim())
}

/// Failures for each (crate, dependency) pair whose dependency is not listed.
pub(crate) fn unlisted(
    dependencies: &BTreeSet<(String, String)>,
    markdown: &str,
) -> Result<Vec<String>, String> {
    let listed = admitted(markdown)?;
    Ok(dependencies
        .iter()
        .filter(|(_, dep)| !listed.contains(dep))
        .map(|(krate, dep)| {
            format!("{krate} depends on {dep}, which docs/dependencies.md does not list")
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceKind {
    Ident(String),
    String(String),
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceToken {
    kind: SourceKind,
    line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSite {
    path: String,
    line: usize,
    context: String,
    operation: String,
}

#[derive(Debug, Clone, Copy)]
struct SourceAllowance {
    path: &'static str,
    context: &'static str,
    operation: &'static str,
    ticket: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct LabelAllowance {
    path: &'static str,
    function: &'static str,
    context: &'static str,
    name: &'static str,
    reason: &'static str,
}

macro_rules! source_allowance {
    ($path:literal, $context:literal, $operation:literal, $ticket:literal) => {
        SourceAllowance {
            path: $path,
            context: $context,
            operation: $operation,
            ticket: $ticket,
        }
    };
}

#[rustfmt::skip]
const SESSION_ALLOWANCES: &[SourceAllowance] = &[
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn revalidate", "join:sessions", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn follow", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn read_session", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn on_left", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn how_left", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log", "join:sessions", "#1725"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log", "join:events.jsonl", "#1725"),
];
#[rustfmt::skip]
const HOME_ALLOWANCES: &[SourceAllowance] = &[
    source_allowance!("crates/cli/src/hub_status.rs", "fn devices|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/cli/src/mcp.rs", "fn rollback|root:home", "join:credentials", "#1720"),
    source_allowance!("crates/cli/src/mcp.rs", "fn record_approval", "config-path:.fiber/config.json", "#1720"),
    source_allowance!("crates/cli/src/prune.rs", "fn prune_run|root:home", "join:logs", "#1720"),
    source_allowance!("crates/cli/src/prune.rs", "fn prune_run|root:home", "join:crashes", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn select|root:home", "join:projects", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn scope_keys|root:home", "join:projects", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn revalidate|root:home", "join:projects", "#1720"),
    source_allowance!("crates/cli/src/login/host.rs", "fn masked_by_project|root:home", "join:projects", "#2909"),
    source_allowance!("crates/doors/src/attach.rs", "fn attach|root:home", "join:run", "#1720"),
    source_allowance!("crates/doors/src/isolation.rs", "fn isolate|root:home", "join:projects", "#1720"),
    source_allowance!("crates/doors/src/hub.rs", "fn connect_with|root:home", "join:run", "#1720"),
    source_allowance!("crates/doors/src/hub.rs", "fn connect_with|root:home/run", "join:hub", "#1720"),
    source_allowance!("crates/doors/src/hub.rs", "fn attempt|root:home", "join:run", "#1720"),
    source_allowance!("crates/doors/src/hub.rs", "fn attempt|root:home/run", "join:hub", "#1720"),
    source_allowance!("crates/doors/src/socket.rs", "fn bind|root:home", "join:run", "#1720"),
    source_allowance!("crates/doors/src/steer.rs", "fn steer|root:home", "join:run", "#1720"),
    source_allowance!("crates/doors/src/watch.rs", "fn watch_after|root:home", "join:run", "#1720"),
    source_allowance!("crates/extensions/src/hooks.rs", "fn sections|root:self.home", "join:data", "#1720"),
    source_allowance!("crates/extensions/src/hooks.rs", "fn sections|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/extensions/src/hooks.rs", "fn sections|root:self.home/projects/project", "join:data", "#1720"),
    source_allowance!("crates/extensions/src/host/fs.rs", "fn install|root:home", "join:data", "#1720"),
    source_allowance!("crates/extensions/src/host/fs.rs", "fn install|root:home", "join:projects", "#1720"),
    source_allowance!("crates/extensions/src/host/fs.rs", "fn install|root:home/projects/key", "join:data", "#1720"),
    source_allowance!("crates/extensions/src/install.rs", "fn stage|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn lock|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn list|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn read_entries|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn locked_entries|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn package_names|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn removal|root:home|scope:None = >::let path = home . join ( extensions ) . join ( slug ( & name ) ? )", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn removal|root:home|scope:if let Some ( dir ) = damaged_dir::dirs . push ( home . join ( extensions ) . join ( & dir ) )", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn removal|root:home|scope:for name in orphan_names::dirs . push ( home . join ( extensions ) . join ( & dir ) )", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/installed.rs", "fn layers|root:home", "join:projects", "#1720"),
    source_allowance!("crates/extensions/src/login.rs", "fn login_provider|root:home", "join:extensions", "#1720"),
    source_allowance!("crates/extensions/src/release.rs", "fn install_release_with|root:home", "join:docs", "#1720"),
    source_allowance!("crates/extensions/src/repository/content.rs", "fn load|root:home", "join:pinned.json", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn approvals|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn approvals|root:self.home/projects/self", "join:approvals", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn approvals|root:self.home", "join:approvals", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn copy_dir|root:self.home", "join:pinned", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn ensure_copy|root:self.home", "join:pinned", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn pin|root:self.home", "join:pinned", "#1720"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn kept_copies|root:home", "join:servers", "#1322"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn kept_copies|root:home", "join:projects", "#653"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn kept_copies|root:home", "join:approvals", "#653"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn kept_copies|root:home", "join:pinned", "#653"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn pinned_lock|root:home", "join:pinned", "#653"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn remove_copy|root:home", "join:pinned", "#653"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn complete|root:home", "join:extensions", "#620"),
    source_allowance!("crates/extensions/src/repository/store.rs", "fn install_depends|root:home", "join:extensions", "#620"),
    source_allowance!("crates/hub/src/closed.rs", "fn running|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn holder|root:home", "join:hub", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn token_hash|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn write_value|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn write_value|root:home", "join:hub", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn sync_dir|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn touch|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn remove|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn exists|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/devices.rs", "fn list|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/diag.rs", "fn open|root:home", "join:logs", "#1720"),
    source_allowance!("crates/hub/src/diag.rs", "fn open|root:home", "join:crashes", "#1720"),
    source_allowance!("crates/hub/src/feed.rs", "fn scan|root:self.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/feed.rs", "fn socket|root:self.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/lib.rs", "fn serve|root:home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/lib.rs", "fn serve|root:home/run", "join:hub", "#1720"),
    source_allowance!("crates/hub/src/listen.rs", "fn open_run|root:home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/listen.rs", "fn bind|root:home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/listen.rs", "fn bind|root:home/run", "join:hub", "#1720"),
    source_allowance!("crates/hub/src/pairing.rs", "fn io_failure|root:hub.home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/pairing.rs", "fn warn|root:hub.home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/pairing.rs", "fn revoke|root:hub.home", "join:hub/devices", "#1720"),
    source_allowance!("crates/hub/src/prompt_history.rs", "fn answer|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/recent.rs", "fn find|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/recent.rs", "fn path|root:home", "join:recent.jsonl", "#1720"),
    source_allowance!("crates/hub/src/rejoin.rs", "fn rejoin_pass|root:hub.home|scope:for candidate in candidates::if ! hub . home . join ( run ) . join ( & session ) . exists ( )", "join:run", "#1720"),
    source_allowance!("crates/hub/src/rejoin.rs", "fn rejoin_pass|root:hub.home|scope:for candidate in candidates::let Ok ( stream ) = UnixStream : : connect ( hub . home . join ( run ) . join ( & session ) ) else", "join:run", "#1720"),
    source_allowance!("crates/hub/src/relay.rs", "fn route|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/resume.rs", "fn reach|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/retire.rs", "fn held_by_exiting|root:hub.home", "join:run", "#2054"),
    source_allowance!("crates/hub/src/rewind.rs", "fn reach|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/sessions_stop.rs", "fn list|root:hub.home", "join:run", "#2959"),
    source_allowance!("crates/hub/src/sessions_stop.rs", "fn connect_now|root:hub.home", "join:run", "#2959"),
    source_allowance!("crates/hub/src/sessions_stop.rs", "fn connect_session|root:hub.home", "join:run", "#2959"),
    source_allowance!("crates/hub/src/start.rs", "fn run|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/log/src/diag.rs", "fn new|root:home", "join:logs", "#1720"),
    source_allowance!("crates/log/src/lib.rs", "fn session_dir|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/lib.rs", "fn sessions_dir|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/name.rs", "fn session_name|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/name.rs", "fn session_dirs|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/read.rs", "fn walk|root:home", "join:projects", "#1725"),
    source_allowance!("crates/log/src/scan.rs", "fn started_sessions|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/escalations/place.rs", "fn sessions|root:home", "join:projects", "#2481"),
    source_allowance!("crates/log/src/search.rs", "fn projects|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/loop/src/skills.rs", "fn places|root:inputs.home", "join:skills", "#1720"),
    source_allowance!("crates/loop/src/skills.rs", "fn places|root:inputs.home", "join:docs/skills", "#1720"),
    source_allowance!("crates/main/src/case/call.rs", "fn provider_extension|root:home|scope:let extension_for = | package : & extensions : : Installed |::let dir = home . join ( extensions ) . join ( config : : dir_name ( & package . name ) )", "join:extensions", "#1720"),
    source_allowance!("crates/main/src/case/call.rs", "fn provider_extension|root:home|scope:installed . iter ( ) . find_map ( | package |::let dir = home . join ( extensions ) . join ( config : : dir_name ( & package . name ) )", "join:extensions", "#1720"),
    source_allowance!("crates/main/src/configure.rs", "fn set|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/crash.rs", "fn write_report|root:home", "join:crashes", "#1720"),
    source_allowance!("crates/main/src/launch.rs", "fn launch|root:home", "join:cache", "#1720"),
    source_allowance!("crates/main/src/main.rs", "fn ask_permissions|root:home", "join:credentials", "#1720"),
    source_allowance!("crates/main/src/mcp_servers.rs", "fn session_tools|root:home", "join:cache", "#1720"),
    source_allowance!("crates/main/src/prompt_files.rs", "fn system", "prompt:SYSTEM.md", "#1720"),
    source_allowance!("crates/main/src/prompt_files.rs", "fn append", "prompt:APPEND_SYSTEM.md", "#1720"),
    source_allowance!("crates/main/src/prompt_files.rs", "fn read_one|root:home", "join:projects", "#1720"),
    source_allowance!("crates/main/src/prompt_files.rs", "fn read_one|root:home/projects/project", "join:prompt-name", "#1720"),
    source_allowance!("crates/main/src/prompt_files.rs", "fn read_one|root:home", "join:prompt-name", "#1720"),
    source_allowance!("crates/main/src/theme_setting.rs", "fn lookup|root:home", "join:themes", "#1720"),
    source_allowance!("crates/main/src/theme_setting.rs", "fn names|root:home", "join:themes", "#1720"),
];
const LABEL_ALLOWANCES: &[LabelAllowance] = &[
    LabelAllowance {
        path: "crates/loop/src/opening.rs",
        function: "render",
        context: "environment-template",
        name: "shell",
        reason: "fills the opening environment template",
    },
    LabelAllowance {
        path: "crates/loop/src/status.rs",
        function: "key",
        context: "state-key",
        name: "jobs",
        reason: "names a session state key",
    },
    LabelAllowance {
        path: "crates/loop/src/skills.rs",
        function: "commands",
        context: "command-tag",
        name: "skill",
        reason: "labels a listed command",
    },
    LabelAllowance {
        path: "crates/loop/src/handoff/carry.rs",
        function: "restart",
        context: "template-key",
        name: "jobs",
        reason: "fills the handoff template",
    },
    LabelAllowance {
        path: "crates/loop/src/reviewer.rs",
        function: "sections",
        context: "prompt-section",
        name: "handoff",
        reason: "selects the handoff section",
    },
];

const HOME_COMPONENTS: &[&str] = &[
    "projects",
    "config",
    "config.json",
    "credentials",
    "data",
    "themes",
    "extensions",
    "cache",
    "logs",
    "crashes",
    "run",
    "hub",
    "recent.jsonl",
    "pinned",
    "pinned.json",
    "approvals",
    "servers",
    "docs",
    "skills",
];

const MAX_HOME_JOIN_WORK: usize = 16_384;
const MAX_HOME_JOIN_CONTEXT_BYTES: usize = 1_048_576;

#[derive(Debug, PartialEq, Eq)]
struct StalledCursor {
    line: usize,
}

/// Keep every scanner collection within the input that produced its entries.
/// `checked_sub` makes the cap independent of a comparison whose mutation could
/// turn a fail-closed bound into an unbounded push.
fn push_bounded<T>(
    values: &mut Vec<T>,
    value: T,
    input_len: usize,
    line: usize,
) -> Result<(), StalledCursor> {
    input_len
        .checked_sub(values.len())
        .and_then(|remaining| remaining.checked_sub(1))
        .ok_or(StalledCursor { line })?;
    values.push(value);
    Ok(())
}

struct HomeJoinBudget {
    steps: usize,
    bytes: usize,
}

impl HomeJoinBudget {
    fn new(steps: usize, bytes: usize) -> Self {
        Self { steps, bytes }
    }

    fn spend_steps(&mut self, amount: usize, line: usize) -> Result<(), StalledCursor> {
        let remaining = self
            .steps
            .checked_sub(amount)
            .ok_or(StalledCursor { line })?;
        if remaining == 0 {
            return Err(StalledCursor { line });
        }
        self.steps = remaining;
        Ok(())
    }

    fn spend_bytes(&mut self, amount: usize, line: usize) -> Result<(), StalledCursor> {
        let remaining = self
            .bytes
            .checked_sub(amount)
            .ok_or(StalledCursor { line })?;
        if remaining == 0 {
            return Err(StalledCursor { line });
        }
        self.bytes = remaining;
        Ok(())
    }
}

struct SourceCursor<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    line: usize,
}

impl<'a> SourceCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.char_indices().peekable(),
            line: 1,
        }
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        self.chars.peek().copied()
    }

    fn offset(&mut self) -> usize {
        self.peek().map_or(self.source.len(), |(offset, _)| offset)
    }

    fn starts_with(&mut self, text: &str) -> bool {
        self.source
            .get(self.offset()..)
            .is_some_and(|remaining| remaining.starts_with(text))
    }

    fn next(&mut self) -> Option<(usize, char)> {
        let next = self.chars.next();
        if next.is_some_and(|(_, character)| character == '\n') {
            self.line += 1;
        }
        next
    }

    fn advance(&mut self) -> Result<(usize, char), StalledCursor> {
        // Inspect the iterator directly: a stalled `peek` must not hide EOF.
        let before = self.chars.peek().map(|(offset, _)| *offset);
        let next = self.next();
        let after = self.chars.peek().map(|(offset, _)| *offset);
        if before == after {
            return Err(StalledCursor { line: self.line });
        }
        next.ok_or(StalledCursor { line: self.line })
    }

    fn consume_to(&mut self, end: usize) -> Result<(), StalledCursor> {
        while self.offset() < end {
            self.advance()?;
        }
        Ok(())
    }

    fn consume(&mut self, text: &str) -> Result<(), StalledCursor> {
        for expected in text.chars() {
            let (_, actual) = self.advance()?;
            debug_assert_eq!(actual, expected);
        }
        Ok(())
    }
}

/// Rust tokens needed by the source gates. Comments disappear, while string
/// literals remain available as path and identity values.
fn source_tokens(source: &str) -> Result<Vec<SourceToken>, StalledCursor> {
    let bytes = source.as_bytes();
    let mut cursor = SourceCursor::new(source);
    let mut tokens = Vec::new();
    while let Some((at, character)) = cursor.peek() {
        if character.is_ascii_whitespace() {
            cursor.advance()?;
            continue;
        }
        if cursor.starts_with("//") {
            cursor.consume("//")?;
            while cursor
                .peek()
                .is_some_and(|(_, character)| character != '\n')
            {
                cursor.advance()?;
            }
            continue;
        }
        if cursor.starts_with("/*") {
            cursor.consume("/*")?;
            let mut nesting = vec![()];
            while !nesting.is_empty() {
                if cursor.starts_with("/*") {
                    cursor.consume("/*")?;
                    push_bounded(&mut nesting, (), bytes.len(), cursor.line)?;
                } else if cursor.starts_with("*/") {
                    cursor.consume("*/")?;
                    nesting.pop();
                } else if cursor.chars.peek().is_none() {
                    break;
                } else {
                    cursor.advance()?;
                }
            }
            continue;
        }
        if let Some(end) = char_literal_end(source, at) {
            cursor.consume_to(end)?;
            continue;
        }
        if let Some((quote, raw, hashes)) = string_start(bytes, at) {
            let token_line = cursor.line;
            cursor.consume_to(quote.saturating_add(1))?;
            let content_start = cursor.offset();
            if raw {
                let closing = format!("\"{}", "#".repeat(hashes));
                while cursor.peek().is_some() && !cursor.starts_with(&closing) {
                    cursor.advance()?;
                }
            } else {
                let mut escaped = false;
                while let Some((_, character)) = cursor.peek() {
                    if character == '"' && !escaped {
                        break;
                    }
                    cursor.advance()?;
                    escaped = character == '\\' && !escaped;
                }
            }
            let content_end = cursor.offset();
            let value = source
                .get(content_start..content_end)
                .unwrap_or_default()
                .to_owned();
            push_bounded(
                &mut tokens,
                SourceToken {
                    kind: SourceKind::String(value),
                    line: token_line,
                },
                bytes.len(),
                token_line,
            )?;
            if cursor.peek().is_some_and(|(_, character)| character == '"') {
                cursor.advance()?;
                if raw {
                    for _ in 0..hashes {
                        cursor.advance()?;
                    }
                }
            }
            continue;
        }
        let token_line = cursor.line;
        if character.is_ascii_alphabetic() || character == '_' {
            let start = at;
            while cursor
                .peek()
                .is_some_and(|(_, next)| next.is_ascii_alphanumeric() || next == '_')
            {
                cursor.advance()?;
            }
            let value = source
                .get(start..cursor.offset())
                .unwrap_or_default()
                .to_owned();
            push_bounded(
                &mut tokens,
                SourceToken {
                    kind: SourceKind::Ident(value),
                    line: token_line,
                },
                bytes.len(),
                token_line,
            )?;
        } else {
            cursor.advance()?;
            push_bounded(
                &mut tokens,
                SourceToken {
                    kind: SourceKind::Punct(character),
                    line: token_line,
                },
                bytes.len(),
                token_line,
            )?;
        }
    }
    Ok(tokens)
}

fn char_literal_end(source: &str, at: usize) -> Option<usize> {
    let mut chars = source.get(at..)?.char_indices().peekable();
    if chars.next()?.1 != '\'' {
        return None;
    }
    let first = chars.next()?.1;
    if first == '\\' {
        match chars.next()?.1 {
            'u' if chars.peek().is_some_and(|(_, character)| *character == '{') => {
                chars.next();
                let mut closed = false;
                for (_, character) in chars.by_ref() {
                    if character == '}' {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return None;
                }
            }
            'x' => {
                chars.next()?;
                chars.next()?;
            }
            _ => {}
        }
    } else if matches!(first, '\'' | '\n') {
        return None;
    }
    let (closing, character) = chars.next()?;
    if character != '\'' {
        return None;
    }
    at.checked_add(closing)?.checked_add(character.len_utf8())
}

/// Start of a normal or raw Rust string; `quote` is the opening quote offset.
fn string_start(bytes: &[u8], at: usize) -> Option<(usize, bool, usize)> {
    for prefix in [b"br".as_slice(), b"cr", b"r", b"b", b"c", b""] {
        if !bytes
            .get(at..)
            .is_some_and(|remaining| remaining.starts_with(prefix))
        {
            continue;
        }
        let Some(prefix_end) = at.checked_add(prefix.len()) else {
            continue;
        };
        let Some(suffix) = bytes.get(prefix_end..) else {
            continue;
        };
        if prefix.ends_with(b"r") {
            let hashes = suffix.iter().take_while(|&&byte| byte == b'#').count();
            let Some(quote) = prefix_end.checked_add(hashes) else {
                continue;
            };
            if bytes.get(quote) == Some(&b'"') {
                return Some((quote, true, hashes));
            }
        } else if suffix.first() == Some(&b'"') {
            return Some((prefix_end, false, 0));
        }
    }
    None
}

fn ident(token: Option<&SourceToken>, word: &str) -> bool {
    matches!(token.map(|t| &t.kind), Some(SourceKind::Ident(value)) if value == word)
}

fn punct(token: Option<&SourceToken>, ch: char) -> bool {
    matches!(token.map(|t| &t.kind), Some(SourceKind::Punct(value)) if *value == ch)
}

fn string(token: Option<&SourceToken>) -> Option<&str> {
    match token.map(|t| &t.kind) {
        Some(SourceKind::String(value)) => Some(value),
        _ => None,
    }
}

fn matching(tokens: &[SourceToken], start: usize, left: char, right: char) -> Option<usize> {
    if !punct(tokens.get(start), left) {
        return None;
    }
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if punct(Some(token), left) {
            depth += 1;
        }
        if punct(Some(token), right) {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn cfg_test_attribute(tokens: &[SourceToken], start: usize, end: usize) -> bool {
    end.checked_sub(start) == Some(4)
        && ident(tokens.get(start), "cfg")
        && punct(tokens.get(start + 1), '(')
        && ident(tokens.get(start + 2), "test")
        && punct(tokens.get(start + 3), ')')
}

fn struct_body(tokens: &[SourceToken], at: usize) -> bool {
    let Some(open) = (0..at).rev().find(|i| {
        punct(tokens.get(*i), '{') && matching(tokens, *i, '{', '}').is_some_and(|end| at < end)
    }) else {
        return false;
    };
    let start = (0..open)
        .rev()
        .find(|i| {
            matches!(
                tokens.get(*i).map(|token| &token.kind),
                Some(SourceKind::Punct(';' | '{' | '}'))
            )
        })
        .map_or(0, |i| i.saturating_add(1));
    (start..open).any(|i| {
        ["struct", "enum", "union"]
            .iter()
            .any(|name| ident(tokens.get(i), name))
    })
}

fn test_item_end(
    tokens: &[SourceToken],
    start: usize,
    field: bool,
) -> Result<usize, StalledCursor> {
    let mut groups = Vec::new();
    for i in start..tokens.len() {
        if groups.is_empty() {
            if punct(tokens.get(i), ';') || (field && punct(tokens.get(i), ',')) {
                return Ok(i);
            }
            if field && punct(tokens.get(i), '}') {
                return Ok(i.saturating_sub(1));
            }
            if punct(tokens.get(i), '{') {
                return Ok(matching(tokens, i, '{', '}').unwrap_or(tokens.len() - 1));
            }
        }
        match tokens.get(i).map(|token| &token.kind) {
            Some(SourceKind::Punct('(')) => {
                push_bounded(
                    &mut groups,
                    ')',
                    tokens.len(),
                    tokens.get(i).map_or(1, |token| token.line),
                )?;
            }
            Some(SourceKind::Punct('[')) => {
                push_bounded(
                    &mut groups,
                    ']',
                    tokens.len(),
                    tokens.get(i).map_or(1, |token| token.line),
                )?;
            }
            Some(SourceKind::Punct('<')) if field => {
                push_bounded(
                    &mut groups,
                    '>',
                    tokens.len(),
                    tokens.get(i).map_or(1, |token| token.line),
                )?;
            }
            Some(SourceKind::Punct(close @ (')' | ']'))) if groups.last() == Some(close) => {
                groups.pop();
            }
            Some(SourceKind::Punct('>')) if groups.last() == Some(&'>') => {
                groups.pop();
            }
            _ => {}
        }
    }
    Ok(tokens.len().saturating_sub(1))
}

fn crate_root_cfg_test(tokens: &[SourceToken]) -> bool {
    if !punct(tokens.first(), '#') {
        return false;
    }
    if !punct(tokens.get(1), '!') {
        return false;
    }
    if !punct(tokens.get(2), '[') {
        return false;
    }
    let Some(end) = matching(tokens, 2, '[', ']') else {
        return false;
    };
    cfg_test_attribute(tokens, 3, end)
}

/// Marks cfg(test)-guarded items, including inline modules and file modules.
fn test_only_tokens(tokens: &[SourceToken]) -> Result<Vec<bool>, StalledCursor> {
    let mut marked = vec![false; tokens.len()];
    if crate_root_cfg_test(tokens) {
        marked.fill(true);
        return Ok(marked);
    }
    for (i, token) in tokens.iter().enumerate() {
        if !punct(Some(token), '#') {
            continue;
        }
        if !punct(tokens.get(i.saturating_add(1)), '[') {
            continue;
        }
        let Some(attr_start) = i.checked_add(1) else {
            continue;
        };
        let Some(attr_end) = matching(tokens, attr_start, '[', ']') else {
            continue;
        };
        if !cfg_test_attribute(tokens, attr_start.saturating_add(1), attr_end) {
            continue;
        }
        let item = attr_end.saturating_add(1);
        let end = test_item_end(tokens, item, struct_body(tokens, i))?;
        for marked_token in marked
            .iter_mut()
            .take(end.saturating_add(1).min(tokens.len()))
            .skip(i)
        {
            *marked_token = true;
        }
    }
    Ok(marked)
}

fn module_target(
    file: &RustFile,
    name: &str,
    tokens: &[SourceToken],
    module_index: usize,
) -> Option<String> {
    let rel = std::path::Path::new(&file.rel);
    let parent = rel.parent()?;
    let default_dir = match rel.file_stem()?.to_str()? {
        "lib" | "main" | "mod" => parent.to_path_buf(),
        stem => parent.join(stem),
    };
    let from = (0..module_index)
        .rfind(|i| punct(tokens.get(*i), ';'))
        .map_or(0, |i| i.saturating_add(1));
    let path_attr = (from..module_index).find_map(|i| {
        (punct(tokens.get(i), '#')
            && punct(tokens.get(i + 1), '[')
            && ident(tokens.get(i + 2), "path")
            && punct(tokens.get(i + 3), '='))
        .then(|| string(tokens.get(i + 4)))
        .flatten()
    });
    let path = if let Some(path) = path_attr {
        parent.join(path)
    } else {
        default_dir.join(format!("{name}.rs"))
    };
    let root = file.path.strip_suffix(&file.rel)?;
    let normalized = path.to_string_lossy().replace('\\', "/");
    Some(format!("{}/{normalized}", root.trim_end_matches('/')))
}

fn stalled_cursor_diagnostic(file: &RustFile, stalled: StalledCursor) -> String {
    format!(
        "{}:{}: source cursor stopped advancing",
        file.path, stalled.line
    )
}

/// Production source files under crate `src/`, excluding conventional tests,
/// cfg(test) items and files referenced only from cfg(test) modules.
fn production_file<'a>(candidates: &[&'a RustFile], path: &str) -> Option<&'a RustFile> {
    candidates.iter().copied().find(|file| file.path == path)
}

fn production_files(files: &[RustFile]) -> Result<Vec<&RustFile>, String> {
    let candidates: Vec<&RustFile> = files
        .iter()
        .filter(|f| f.krate != "fakes" && f.rel.starts_with("src/") && !is_test_file(&f.rel))
        .collect();
    let paths: BTreeSet<&str> = candidates.iter().map(|f| f.path.as_str()).collect();
    let mut edges = Vec::new();
    let mut token_budget = 0usize;
    for file in &candidates {
        let tokens = source_tokens(&file.source)
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        let tests = test_only_tokens(&tokens)
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        token_budget = token_budget
            .checked_add(tokens.len())
            .ok_or_else(|| stalled_cursor_diagnostic(file, StalledCursor { line: 1 }))?;
        for i in 0..tokens.len().saturating_sub(2) {
            if !ident(tokens.get(i), "mod") || !punct(tokens.get(i + 2), ';') {
                continue;
            }
            let Some(name) = tokens.get(i + 1).and_then(|token| match &token.kind {
                SourceKind::Ident(name) => Some(name.as_str()),
                SourceKind::String(_) | SourceKind::Punct(_) => None,
            }) else {
                continue;
            };
            if let Some(target) = module_target(file, name, &tokens, i)
                && paths.contains(target.as_str())
            {
                push_bounded(
                    &mut edges,
                    (
                        file.path.as_str(),
                        target,
                        tests.get(i).copied().unwrap_or(false),
                    ),
                    token_budget,
                    tokens.get(i).map_or(1, |token| token.line),
                )
                .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
            }
        }
    }
    let mut production = BTreeSet::new();
    let mut pending = Vec::new();
    for file in &candidates {
        let has_parent = edges.iter().any(|(_, target, _)| target == &file.path);
        if !has_parent && production.insert(file.path.as_str()) {
            push_bounded(&mut pending, file.path.as_str(), candidates.len(), 1)
                .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        }
    }
    // Each candidate is enqueued once; cap both visits and queued paths if that invariant breaks.
    for _ in 0..=candidates.len() {
        let Some(parent) = pending.pop() else {
            break;
        };
        for (source, target, test_edge) in &edges {
            if *source != parent {
                continue;
            }
            if *test_edge {
                continue;
            }
            if production.insert(target.as_str()) {
                let file = production_file(&candidates, parent);
                let line = 1;
                push_bounded(&mut pending, target.as_str(), candidates.len(), line).map_err(
                    |stalled| {
                        if let Some(file) = file {
                            stalled_cursor_diagnostic(file, stalled)
                        } else {
                            format!("{parent}:{}: source cursor stopped advancing", stalled.line)
                        }
                    },
                )?;
            }
        }
    }
    if let Some(parent) = pending.last().copied() {
        if let Some(file) = production_file(&candidates, parent) {
            return Err(stalled_cursor_diagnostic(file, StalledCursor { line: 1 }));
        }
        return Err(format!("{parent}:1: source cursor stopped advancing"));
    }
    Ok(candidates
        .into_iter()
        .filter(|file| production.contains(file.path.as_str()))
        .collect())
}

fn function_context(tokens: &[SourceToken], at: usize) -> String {
    for i in (0..at).rev() {
        if !ident(tokens.get(i), "fn") {
            continue;
        }
        let Some(name) = (match tokens.get(i + 1).map(|t| &t.kind) {
            Some(SourceKind::Ident(name)) => Some(name),
            _ => None,
        }) else {
            continue;
        };
        let Some(open) = (i + 2..tokens.len())
            .find(|j| punct(tokens.get(*j), '{') || punct(tokens.get(*j), ';'))
        else {
            continue;
        };
        if !punct(tokens.get(open), '{') {
            continue;
        }
        if matching(tokens, open, '{', '}').is_some_and(|end| at >= open && at <= end) {
            return format!("fn {name}");
        }
    }
    "module".to_owned()
}

fn scope_context(tokens: &[SourceToken], at: usize) -> String {
    let open = (0..at).rev().find(|i| {
        punct(tokens.get(*i), '{') && matching(tokens, *i, '{', '}').is_some_and(|end| at <= end)
    });
    let Some(open) = open else {
        return "module".to_owned();
    };
    let boundary = |i: usize| {
        punct(tokens.get(i), ';') || punct(tokens.get(i), '{') || punct(tokens.get(i), '}')
    };
    let header_start = (0..open).rev().find(|i| boundary(*i)).map_or(0, |i| i + 1);
    let statement_start = (0..at).rev().find(|i| boundary(*i)).map_or(0, |i| i + 1);
    let statement_end = (at..tokens.len())
        .find(|i| boundary(*i))
        .unwrap_or(tokens.len());
    let text = |range: std::ops::Range<usize>| {
        let mut text = String::new();
        for token in tokens.get(range).unwrap_or_default() {
            if !text.is_empty() {
                text.push(' ');
            }
            match &token.kind {
                SourceKind::Ident(value) | SourceKind::String(value) => text.push_str(value),
                SourceKind::Punct(value) => text.push(*value),
            }
        }
        text
    };
    format!(
        "{}::{}",
        text(header_start..open),
        text(statement_start..statement_end)
    )
}

fn home_scope_context(
    file: &RustFile,
    tokens: &[SourceToken],
    at: usize,
    context: String,
    operation: &str,
) -> String {
    let prefix = format!("{context}|scope:");
    if HOME_ALLOWANCES.iter().any(|allowance| {
        allowance.path == file.path
            && allowance.operation == operation
            && allowance.context.starts_with(&prefix)
    }) {
        format!("{prefix}{}", scope_context(tokens, at))
    } else {
        context
    }
}

fn push_source_site(
    file: &RustFile,
    tokens: &[SourceToken],
    sites: &mut Vec<SourceSite>,
    site: SourceSite,
) -> Result<(), String> {
    let line = site.line;
    push_bounded(sites, site, tokens.len(), line)
        .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))
}

fn push_source_failure(
    file: &RustFile,
    input_tokens: usize,
    failures: &mut Vec<String>,
    line: usize,
    failure: String,
) -> Result<(), String> {
    push_bounded(failures, failure, input_tokens, line)
        .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))
}

fn joined_literal(tokens: &[SourceToken], at: usize) -> bool {
    at >= 3
        && punct(tokens.get(at - 3), '.')
        && ident(tokens.get(at - 2), "join")
        && punct(tokens.get(at - 1), '(')
}

#[cfg(test)]
fn home_join_context(
    tokens: &[SourceToken],
    join_dot: usize,
) -> Result<Option<String>, StalledCursor> {
    home_join_context_with_budget(
        tokens,
        join_dot,
        &mut HomeJoinBudget::new(MAX_HOME_JOIN_WORK, MAX_HOME_JOIN_CONTEXT_BYTES),
    )
}

fn home_join_context_with_budget(
    tokens: &[SourceToken],
    join_dot: usize,
    budget: &mut HomeJoinBudget,
) -> Result<Option<String>, StalledCursor> {
    let line = tokens.get(join_dot).map_or(1, |token| token.line);
    let mut join_dot = join_dot;
    let mut segments: Vec<String> = Vec::new();
    for _ in 0..=tokens.len() {
        budget.spend_steps(1, line)?;
        let Some(end) = join_dot.checked_sub(1) else {
            return Ok(None);
        };
        if ident(tokens.get(end), "home") {
            let owner = if previous_punct(tokens, end, 1, '.') {
                match end.checked_sub(2).and_then(|index| tokens.get(index)) {
                    Some(SourceToken {
                        kind: SourceKind::Ident(owner),
                        ..
                    }) if matches!(owner.as_str(), "self" | "hub" | "inputs") => {
                        format!("{owner}.home")
                    }
                    _ => return Ok(None),
                }
            } else {
                "home".to_owned()
            };
            let mut context = owner;
            budget.spend_bytes(context.len(), line)?;
            for segment in segments.iter().rev() {
                budget.spend_bytes(segment.len().saturating_add(1), line)?;
                context.push('/');
                context.push_str(segment);
            }
            return Ok(Some(context));
        }
        if !punct(tokens.get(end), ')') {
            return Ok(None);
        }
        let mut depth = 0usize;
        let mut open = None;
        for i in (0..=end).rev() {
            budget.spend_steps(1, line)?;
            if punct(tokens.get(i), ')') {
                depth = depth.saturating_add(1);
            }
            if punct(tokens.get(i), '(') {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    open = Some(i);
                    break;
                }
            }
        }
        let Some(open) = open else {
            return Ok(None);
        };
        let Some(previous_dot) = open.checked_sub(2) else {
            return Ok(None);
        };
        if !open
            .checked_sub(1)
            .is_some_and(|index| ident(tokens.get(index), "join"))
            || !punct(tokens.get(previous_dot), '.')
        {
            return Ok(None);
        }
        let segment = open
            .checked_add(1)
            .and_then(|index| tokens.get(index))
            .and_then(|token| {
                string(Some(token)).or(match &token.kind {
                    SourceKind::Ident(value) => Some(value.as_str()),
                    SourceKind::String(_) | SourceKind::Punct(_) => None,
                })
            })
            .unwrap_or("*");
        budget.spend_bytes(segment.len(), line)?;
        push_bounded(&mut segments, segment.to_owned(), tokens.len(), line)?;
        if previous_dot >= join_dot {
            return Err(StalledCursor { line });
        }
        join_dot = previous_dot;
    }
    Err(StalledCursor { line })
}

#[cfg(test)]
fn fiber_home_join(
    tokens: &[SourceToken],
    join_dot: usize,
) -> Result<Option<String>, StalledCursor> {
    fiber_home_join_with_budget(
        tokens,
        join_dot,
        &mut HomeJoinBudget::new(MAX_HOME_JOIN_WORK, MAX_HOME_JOIN_CONTEXT_BYTES),
    )
}

fn fiber_home_join_with_budget(
    tokens: &[SourceToken],
    join_dot: usize,
    budget: &mut HomeJoinBudget,
) -> Result<Option<String>, StalledCursor> {
    let Some(context) = home_join_context_with_budget(tokens, join_dot, budget)? else {
        return Ok(None);
    };
    Ok((!context.contains("/.agents") && !context.contains("/.config")).then_some(context))
}

fn path_segment(value: &str, wanted: &str) -> bool {
    value.split('/').any(|part| part == wanted)
}

fn file_operation_near(tokens: &[SourceToken], at: usize) -> bool {
    let operations = [
        "read",
        "read_to_string",
        "read_dir",
        "read_lossy",
        "read_one",
        "open",
        "write",
        "write_all",
        "remove_file",
        "remove_dir",
        "remove_dir_all",
        "create_dir",
        "create_dir_all",
        "metadata",
        "new",
        "from",
    ];
    for i in at.saturating_sub(32)..at {
        if !operations
            .iter()
            .any(|operation| ident(tokens.get(i), operation))
        {
            continue;
        }
        if !punct(tokens.get(i + 1), '(') {
            continue;
        }
        if matching(tokens, i + 1, '(', ')').is_some_and(|end| end >= at) {
            return true;
        }
    }
    false
}

fn session_sites_with_tokens(
    file: &RustFile,
    tokens: &[SourceToken],
) -> Result<Vec<SourceSite>, String> {
    let test_only =
        test_only_tokens(tokens).map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
    let mut sites = Vec::new();
    if file.krate == "log" {
        return Ok(sites);
    }
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(value) = string(Some(token)) else {
            continue;
        };
        let join = joined_literal(tokens, i);
        let operation = if value == "sessions" && join {
            Some("join:sessions")
        } else if value == "session.lock" && join {
            Some("join:session.lock")
        } else if path_segment(value, "sessions") && (join || file_operation_near(tokens, i)) {
            Some("path:sessions")
        } else if value.rsplit('/').next() == Some("events.jsonl")
            && (join || file_operation_near(tokens, i))
        {
            Some("join:events.jsonl")
        } else {
            None
        };
        if let Some(operation) = operation {
            push_source_site(
                file,
                tokens,
                &mut sites,
                SourceSite {
                    path: file.path.clone(),
                    line: token.line,
                    context: function_context(tokens, i),
                    operation: operation.to_owned(),
                },
            )?;
        }
    }
    Ok(sites)
}

fn prompt_literal(value: &str) -> bool {
    matches!(value, "SYSTEM.md" | "APPEND_SYSTEM.md")
}

#[cfg(test)]
fn home_sites(file: &RustFile) -> Result<Vec<SourceSite>, String> {
    let tokens =
        source_tokens(&file.source).map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
    home_sites_with_tokens(file, &tokens)
}

fn home_sites_with_tokens(
    file: &RustFile,
    tokens: &[SourceToken],
) -> Result<Vec<SourceSite>, String> {
    let test_only =
        test_only_tokens(tokens).map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
    let mut sites = Vec::new();
    let mut join_budget = HomeJoinBudget::new(MAX_HOME_JOIN_WORK, MAX_HOME_JOIN_CONTEXT_BYTES);
    if file.krate == "config" {
        return Ok(sites);
    }
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(value) = string(Some(token)) else {
            continue;
        };
        let join = joined_literal(tokens, i);
        let first = value.split('/').next().unwrap_or_default();
        let home_component = HOME_COMPONENTS.contains(&first);
        let prompt_candidate = prompt_literal(value);
        let home_context = if join && (home_component || prompt_candidate) {
            fiber_home_join_with_budget(tokens, i - 3, &mut join_budget)
                .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?
        } else {
            None
        };
        let home_root = home_context.is_some();
        let config_path =
            first == ".fiber" && matches!(value.split('/').nth(1), Some("config.json" | "config"));
        let prompt = prompt_candidate && (home_root || file_operation_near(tokens, i));
        let direct_config = config_path && (join || file_operation_near(tokens, i));
        let operation = if home_root && home_component {
            Some(format!("join:{value}"))
        } else if prompt {
            Some(format!("prompt:{value}"))
        } else if direct_config {
            Some(format!("config-path:{value}"))
        } else {
            None
        };
        if let Some(operation) = operation {
            let context = home_context.map_or_else(
                || function_context(tokens, i),
                |root| format!("{}|root:{root}", function_context(tokens, i)),
            );
            let context = home_scope_context(file, tokens, i, context, &operation);
            push_source_site(
                file,
                tokens,
                &mut sites,
                SourceSite {
                    path: file.path.clone(),
                    line: token.line,
                    context,
                    operation,
                },
            )?;
        }
    }
    for (i, (window, test_window)) in tokens.windows(5).zip(test_only.windows(5)).enumerate() {
        if !punct(window.first(), '.')
            || !ident(window.get(1), "join")
            || !punct(window.get(2), '(')
            || !ident(window.get(3), "name")
            || !punct(window.get(4), ')')
            || test_window.get(3).copied().unwrap_or(false)
            || function_context(tokens, i) != "fn read_one"
        {
            continue;
        }
        let Some(root) = fiber_home_join_with_budget(tokens, i, &mut join_budget)
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?
        else {
            continue;
        };
        let Some(token) = window.get(3) else {
            continue;
        };
        let operation = "join:prompt-name";
        let context = home_scope_context(
            file,
            tokens,
            i,
            format!("fn read_one|root:{root}"),
            operation,
        );
        push_source_site(
            file,
            tokens,
            &mut sites,
            SourceSite {
                path: file.path.clone(),
                line: token.line,
                context,
                operation: operation.to_owned(),
            },
        )?;
    }
    Ok(sites)
}

fn consume_next_allowance(
    remaining: &mut Vec<SourceAllowance>,
    sites: &mut impl Iterator<Item = SourceSite>,
) -> bool {
    let Some(site) = sites.next() else {
        return false;
    };
    let Some(position) = remaining.iter().position(|allowance| {
        allowance.path == site.path
            && allowance.context == site.context
            && allowance.operation == site.operation
    }) else {
        return false;
    };
    let ticket = remaining.remove(position).ticket;
    ticket.strip_prefix('#').is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
fn consume_allowance(allowances: &[SourceAllowance], site: &SourceSite) -> bool {
    let mut remaining = allowances.to_vec();
    consume_next_allowance(&mut remaining, &mut std::iter::once(site.clone()))
}

fn source_failures(files: &[RustFile], allowances: &[SourceAllowance], home: bool) -> Vec<String> {
    let mut remaining = allowances.to_vec();
    let mut failures = Vec::new();
    let mut token_budget = 0usize;
    let production = match production_files(files) {
        Ok(production) => production,
        Err(failure) => return vec![failure],
    };
    for file in production {
        let tokens = match source_tokens(&file.source) {
            Ok(tokens) => tokens,
            Err(stalled) => return vec![stalled_cursor_diagnostic(file, stalled)],
        };
        token_budget = match token_budget.checked_add(tokens.len()) {
            Some(budget) => budget,
            None => return vec![stalled_cursor_diagnostic(file, StalledCursor { line: 1 })],
        };
        let scan = if home {
            home_sites_with_tokens(file, &tokens)
        } else {
            session_sites_with_tokens(file, &tokens)
        };
        let sites = match scan {
            Ok(sites) => sites,
            Err(failure) => {
                if let Err(stalled) =
                    push_source_failure(file, token_budget, &mut failures, 1, failure)
                {
                    return vec![stalled];
                }
                continue;
            }
        };
        for site in sites {
            if !consume_next_allowance(&mut remaining, &mut std::iter::once(site.clone())) {
                let message = if home {
                    "Fiber-home path outside config"
                } else {
                    "session path outside log"
                };
                if let Err(stalled) = push_source_failure(
                    file,
                    token_budget,
                    &mut failures,
                    site.line,
                    format!("{}:{}: {message}", site.path, site.line),
                ) {
                    return vec![stalled];
                }
            }
        }
    }
    failures
}

/// Session-directory constructions and event-log paths outside `log`.
pub(crate) fn session_paths(files: &[RustFile]) -> Vec<String> {
    source_failures(files, SESSION_ALLOWANCES, false)
}

/// Fiber-home layout constructions and configuration-file reads outside `config`.
pub(crate) fn fiber_home_paths(files: &[RustFile]) -> Vec<String> {
    source_failures(files, HOME_ALLOWANCES, true)
}

fn owned_tool_definition(tokens: &[SourceToken], i: usize) -> Option<String> {
    if !ident(tokens.get(i), "name") || !punct(tokens.get(i + 1), ':') {
        return None;
    }
    if let Some(name) = string(tokens.get(i + 2))
        && punct(tokens.get(i + 3), '.')
        && ident(tokens.get(i + 4), "to_owned")
        && punct(tokens.get(i + 5), '(')
        && punct(tokens.get(i + 6), ')')
    {
        return Some(name.to_owned());
    }
    None
}

fn unrecognized_tool_definition(tokens: &[SourceToken], i: usize) -> bool {
    if !ident(tokens.get(i), "name") || !punct(tokens.get(i + 1), ':') {
        return false;
    }
    let value = i + 2;
    (ident(tokens.get(value), "String")
        && punct(tokens.get(value + 1), ':')
        && punct(tokens.get(value + 2), ':')
        && ident(tokens.get(value + 3), "from"))
        || (string(tokens.get(value)).is_some()
            && punct(tokens.get(value + 1), '.')
            && ["to_string", "into"]
                .iter()
                .any(|method| ident(tokens.get(value + 2), method)))
}

fn derived_tool_names(files: &[RustFile]) -> Result<BTreeSet<String>, String> {
    let mut names = Vec::new();
    let mut token_budget = 0usize;
    for file in production_files(files)?
        .into_iter()
        .filter(|file| matches!(file.krate.as_str(), "tools" | "jobs"))
    {
        let tokens = source_tokens(&file.source)
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        token_budget = token_budget
            .checked_add(tokens.len())
            .ok_or_else(|| stalled_cursor_diagnostic(file, StalledCursor { line: 1 }))?;
        let test_only = test_only_tokens(&tokens)
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        for i in 0..tokens.len() {
            if test_only.get(i).copied().unwrap_or(false) {
                continue;
            }
            if let Some(name) = owned_tool_definition(&tokens, i) {
                push_bounded(
                    &mut names,
                    name,
                    token_budget,
                    tokens.get(i).map_or(1, |token| token.line),
                )
                .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn previous_punct(tokens: &[SourceToken], at: usize, offset: usize, wanted: char) -> bool {
    at.checked_sub(offset)
        .is_some_and(|index| punct(tokens.get(index), wanted))
}

fn next_punct(tokens: &[SourceToken], at: usize, offset: usize, wanted: char) -> bool {
    punct(tokens.get(at.saturating_add(offset)), wanted)
}

fn next_ident(tokens: &[SourceToken], at: usize, offset: usize, wanted: &str) -> bool {
    ident(tokens.get(at.saturating_add(offset)), wanted)
}

fn label_context(
    file: &RustFile,
    tokens: &[SourceToken],
    i: usize,
    name: &str,
) -> Option<(&'static str, &'static str)> {
    let context = function_context(tokens, i);
    let path = file.path.as_str();
    if path == "crates/loop/src/opening.rs"
        && context == "fn render"
        && name == "shell"
        && previous_punct(tokens, i, 1, '(')
        && next_punct(tokens, i, 1, ',')
        && next_ident(tokens, i, 2, "environment")
        && next_punct(tokens, i, 3, '.')
        && next_ident(tokens, i, 4, "shell")
    {
        return Some(("render", "fills the opening environment template"));
    }
    if path == "crates/loop/src/status.rs"
        && context == "fn key"
        && name == "jobs"
        && previous_punct(tokens, i, 1, '(')
        && previous_punct(tokens, i, 2, '>')
        && previous_punct(tokens, i, 3, '=')
        && next_punct(tokens, i, 1, ',')
        && next_ident(tokens, i, 2, "None")
    {
        return Some(("key", "names a session state key"));
    }
    if path == "crates/loop/src/skills.rs"
        && context == "fn commands"
        && name == "skill"
        && previous_punct(tokens, i, 1, '{')
        && previous_ident(tokens, i, 2, "model_invocable")
        && next_punct(tokens, i, 1, '}')
        && next_ident(tokens, i, 2, "else")
    {
        return Some(("commands", "labels a listed command"));
    }
    if path == "crates/loop/src/handoff/carry.rs"
        && context == "fn restart"
        && name == "jobs"
        && previous_punct(tokens, i, 1, '(')
        && next_punct(tokens, i, 1, ',')
        && next_ident(tokens, i, 2, "listed")
        && next_punct(tokens, i, 3, '.')
        && next_ident(tokens, i, 4, "join")
    {
        return Some(("restart", "fills the handoff template"));
    }
    if path == "crates/loop/src/reviewer.rs"
        && context == "fn sections"
        && name == "handoff"
        && previous_punct(tokens, i, 1, ',')
        && previous_ident(tokens, i, 2, "md")
        && previous_ident(tokens, i, 4, "section")
        && previous_ident(tokens, i, 7, "prompt")
        && previous_ident(tokens, i, 10, "crate")
        && previous_ident(tokens, i, 12, "handoff")
    {
        return Some(("sections", "selects the handoff section"));
    }
    None
}

fn previous_ident(tokens: &[SourceToken], at: usize, offset: usize, wanted: &str) -> bool {
    at.checked_sub(offset)
        .is_some_and(|index| ident(tokens.get(index), wanted))
}

fn label_allowance_matches(
    allowance: &LabelAllowance,
    path: &str,
    function: &str,
    context: &str,
    name: &str,
    reason: &str,
) -> bool {
    allowance.path == path
        && allowance.function == function
        && allowance.context == context
        && allowance.name == name
        && allowance.reason == reason
}

#[cfg(test)]
fn loop_sites(
    file: &RustFile,
    names: &BTreeSet<String>,
) -> Result<(Vec<SourceSite>, Vec<String>), String> {
    let tokens =
        source_tokens(&file.source).map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
    loop_sites_with_tokens(file, &tokens, names)
}

fn loop_sites_with_tokens(
    file: &RustFile,
    tokens: &[SourceToken],
    names: &BTreeSet<String>,
) -> Result<(Vec<SourceSite>, Vec<String>), String> {
    let test_only =
        test_only_tokens(tokens).map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
    let mut label_allowances = LABEL_ALLOWANCES.to_vec();
    let mut sites = Vec::new();
    let mut failures = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        if matches!(file.krate.as_str(), "tools" | "jobs")
            && unrecognized_tool_definition(tokens, i)
        {
            push_bounded(
                &mut failures,
                format!(
                    "{}:{}: unrecognized owned tool-name definition",
                    file.path, token.line
                ),
                tokens.len(),
                token.line,
            )
            .map_err(|stalled| stalled_cursor_diagnostic(file, stalled))?;
        }
        if file.krate != "loop" {
            continue;
        }
        let Some(name) = string(Some(token)).filter(|name| names.contains(*name)) else {
            continue;
        };
        let mut context = function_context(tokens, i);
        if let Some((function, reason)) = label_context(file, tokens, i, name) {
            let label = match (file.path.as_str(), name) {
                ("crates/loop/src/opening.rs", "shell") => Some("environment-template"),
                ("crates/loop/src/status.rs", "jobs") => Some("state-key"),
                ("crates/loop/src/skills.rs", "skill") => Some("command-tag"),
                ("crates/loop/src/handoff/carry.rs", "jobs") => Some("template-key"),
                ("crates/loop/src/reviewer.rs", "handoff") => Some("prompt-section"),
                _ => None,
            };
            let exception = label.is_some_and(|label| {
                let matching = label_allowances.iter().position(|allowance| {
                    label_allowance_matches(
                        allowance,
                        file.path.as_str(),
                        function,
                        label,
                        name,
                        reason,
                    )
                });
                if let Some(index) = matching {
                    label_allowances.remove(index);
                    true
                } else {
                    false
                }
            });
            if exception {
                continue;
            }
            context = format!("{context} | {reason}");
        }
        push_source_site(
            file,
            tokens,
            &mut sites,
            SourceSite {
                path: file.path.clone(),
                line: token.line,
                context,
                operation: format!("tool:{name}"),
            },
        )?;
    }
    Ok((sites, failures))
}

/// Built-in tool identities named by loop production source.
pub(crate) fn loop_tool_names(files: &[RustFile]) -> Vec<String> {
    let production = match production_files(files) {
        Ok(production) => production,
        Err(failure) => return vec![failure],
    };
    let names = match derived_tool_names(files) {
        Ok(names) => names,
        Err(failure) => return vec![failure],
    };
    let mut failures = Vec::new();
    let mut token_budget = 0usize;
    for file in production {
        let tokens = match source_tokens(&file.source) {
            Ok(tokens) => tokens,
            Err(stalled) => return vec![stalled_cursor_diagnostic(file, stalled)],
        };
        token_budget = match token_budget.checked_add(tokens.len()) {
            Some(budget) => budget,
            None => return vec![stalled_cursor_diagnostic(file, StalledCursor { line: 1 })],
        };
        let (sites, definitions) = match loop_sites_with_tokens(file, &tokens, &names) {
            Ok(result) => result,
            Err(failure) => {
                if let Err(stalled) =
                    push_source_failure(file, token_budget, &mut failures, 1, failure)
                {
                    return vec![stalled];
                }
                continue;
            }
        };
        for definition in definitions {
            if let Err(stalled) =
                push_source_failure(file, token_budget, &mut failures, 1, definition)
            {
                return vec![stalled];
            }
        }
        for site in sites {
            if let Err(stalled) = push_source_failure(
                file,
                token_budget,
                &mut failures,
                site.line,
                format!(
                    "{}:{}: built-in tool name {}",
                    site.path,
                    site.line,
                    site.operation.trim_start_matches("tool:")
                ),
            ) {
                return vec![stalled];
            }
        }
    }
    failures
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
