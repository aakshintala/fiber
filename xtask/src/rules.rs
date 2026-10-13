//! Checks that the code matches the rules and tables in the docs: the
//! 800-line report (`docs/code-quality.md`, "Size"), the `unsafe` table in
//! `docs/code-quality.md`, the crate list in `docs/dependencies.md`, and
//! that process signals stay in the guarded helpers.

use std::collections::BTreeSet;

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
/// `tree` is the output of `cargo tree --prefix depth -e features -i
/// serde_json`: every line starts with its depth, so the package lines
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

/// Files that may send a process signal. An entry must refuse an id of 1 or
/// less before anything runs: group -1 reaches every process the user owns.
const SIGNAL_ALLOWLIST: &[&str] = &[
    "crates/extensions/src/host/exec.rs",
    "crates/fakes/src/process_group.rs",
    "crates/cli/src/extension_test.rs",
    "crates/main/src/switch/read.rs",
    "crates/tui/src/paste_image.rs",
    "crates/support/src/group.rs",
];

/// What counts as a process signal: the pattern, and whether it only counts
/// beside the word kill, on its own line or in the backslash-continued
/// lines around it. A bare double dash also starts an operand such as
/// find's `-delete`, so the shell fragments need their kill. The strings
/// are joined from halves so this file holds no pattern itself; otherwise
/// the check would fail on its own source.
const SIGNAL_PATTERNS: &[(&str, bool)] = &[
    (concat!("Command::", "new(\"kill\")"), false),
    (concat!("--", " -"), true),
    (concat!("\"-", "$"), true),
    (concat!("kill_process_", "group"), false),
    (concat!("kill_process", "("), false),
    (concat!("kill", "pg"), false),
    (concat!("libc::", "kill"), false),
    (concat!("nix::sys::", "signal"), false),
];

pub(crate) fn signal_sites(files: &[RustFile]) -> Vec<String> {
    let mut failures = Vec::new();
    for f in files {
        if SIGNAL_ALLOWLIST.contains(&f.path.as_str()) {
            continue;
        }
        let lines: Vec<&str> = f.source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for &(pattern, needs_kill) in SIGNAL_PATTERNS {
                if !line.contains(pattern) {
                    continue;
                }
                if needs_kill && !chain_has_kill(&lines, index) {
                    continue;
                }
                failures.push(format!("{}:{}: {pattern}", f.path, index + 1));
            }
        }
    }
    failures
}

/// Whether `line` continues a shell command from the line before it: the
/// earlier text ends with a backslash, possibly followed by the closing
/// quote or a newline escape.
fn continues(line: &str) -> bool {
    let trimmed = line.trim_end();
    let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("\\n").unwrap_or(trimmed);
    trimmed.ends_with('\\')
}

/// Whether the word kill is on the fragment's own line or on an earlier line
/// the fragment continues through backslashes. A command names `kill` before
/// its arguments, so only earlier lines count.
fn chain_has_kill(lines: &[&str], index: usize) -> bool {
    let (earlier, rest) = lines.split_at(index.min(lines.len()));
    let earlier = earlier.iter().rev().take_while(|line| continues(line));
    rest.iter()
        .take(1)
        .chain(earlier)
        .any(|line| line.contains("kill"))
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
    source_allowance!("crates/hub/src/delete.rs", "fn take_lock", "join:session.lock", "#1725"),
    source_allowance!("crates/hub/src/delete.rs", "fn remove", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn follow", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn read_session", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn on_left", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/feed.rs", "fn how_left", "join:events.jsonl", "#1725"),
    source_allowance!("crates/hub/src/recent.rs", "fn session_dir", "join:sessions", "#1725"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log", "join:sessions", "#1725"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log", "join:events.jsonl", "#1725"),
    source_allowance!("crates/jobs/src/delegate.rs", "fn run", "join:events.jsonl", "#1725"),
    source_allowance!("crates/main/src/resume.rs", "fn resumed_session", "join:events.jsonl", "#1725"),
    source_allowance!("crates/main/src/session_command.rs", "fn run_new", "join:events.jsonl", "#1725"),
];
#[rustfmt::skip]
const HOME_ALLOWANCES: &[SourceAllowance] = &[
    source_allowance!("crates/cli/src/hub_status.rs", "fn port|root:home", "join:config.json", "#1720"),
    source_allowance!("crates/cli/src/hub_status.rs", "fn devices|root:home", "join:hub/devices", "#1720"),
    source_allowance!("crates/cli/src/mcp.rs", "fn rollback|root:home", "join:credentials", "#1720"),
    source_allowance!("crates/cli/src/mcp.rs", "fn record_approval", "config-path:.fiber/config.json", "#1720"),
    source_allowance!("crates/cli/src/prune.rs", "fn prune_run|root:home", "join:logs", "#1720"),
    source_allowance!("crates/cli/src/prune.rs", "fn prune_run|root:home", "join:crashes", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn select|root:home", "join:projects", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn scope_keys|root:home", "join:projects", "#1720"),
    source_allowance!("crates/cli/src/prune/worktrees.rs", "fn revalidate|root:home", "join:projects", "#1720"),
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
    source_allowance!("crates/hub/src/recent.rs", "fn session_dir|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/recent.rs", "fn find|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/recent.rs", "fn path|root:home", "join:recent.jsonl", "#1720"),
    source_allowance!("crates/hub/src/rejoin.rs", "fn rejoin_pass|root:hub.home|scope:for candidate in candidates::if ! hub . home . join ( run ) . join ( & session ) . exists ( )", "join:run", "#1720"),
    source_allowance!("crates/hub/src/rejoin.rs", "fn rejoin_pass|root:hub.home|scope:for candidate in candidates::let Ok ( stream ) = UnixStream : : connect ( hub . home . join ( run ) . join ( & session ) ) else", "join:run", "#1720"),
    source_allowance!("crates/hub/src/relay.rs", "fn route|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/resume.rs", "fn reach|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/resume.rs", "fn find_log|root:home", "join:projects", "#1720"),
    source_allowance!("crates/hub/src/rewind.rs", "fn reach|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/hub/src/start.rs", "fn run|root:hub.home", "join:run", "#1720"),
    source_allowance!("crates/log/src/dependents.rs", "fn pointers|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/diag.rs", "fn new|root:home", "join:logs", "#1720"),
    source_allowance!("crates/log/src/lib.rs", "fn sessions_dir|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/name.rs", "fn session_name|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/scan.rs", "fn started_sessions|root:home", "join:projects", "#1720"),
    source_allowance!("crates/log/src/search.rs", "fn projects|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/loop/src/skills.rs", "fn places|root:inputs.home", "join:skills", "#1720"),
    source_allowance!("crates/loop/src/skills.rs", "fn places|root:inputs.home", "join:docs/skills", "#1720"),
    source_allowance!("crates/main/src/case/call.rs", "fn provider_extension|root:home|scope:let extension_for = | package : & extensions : : Installed |::let dir = home . join ( extensions ) . join ( config : : dir_name ( & package . name ) )", "join:extensions", "#1720"),
    source_allowance!("crates/main/src/case/call.rs", "fn provider_extension|root:home|scope:installed . iter ( ) . find_map ( | package |::let dir = home . join ( extensions ) . join ( config : : dir_name ( & package . name ) )", "join:extensions", "#1720"),
    source_allowance!("crates/main/src/case/run.rs", "fn write_inputs|root:home", "join:config.json", "#1720"),
    source_allowance!("crates/main/src/configure.rs", "fn set|root:self.home", "join:projects", "#1720"),
    source_allowance!("crates/main/src/configure.rs", "fn set|root:self.home/projects/project", "join:config.json", "#1720"),
    source_allowance!("crates/main/src/configure.rs", "fn global_file|root:self.home", "join:config.json", "#1720"),
    source_allowance!("crates/main/src/crash.rs", "fn write_report|root:home", "join:crashes", "#1720"),
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
    "docs",
    "skills",
];

/// Rust tokens needed by the source gates. Comments disappear, while string
/// literals remain available as path and identity values.
fn source_tokens(source: &str) -> Vec<SourceToken> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    let mut line = 1;
    while at < bytes.len() {
        let Some(byte) = bytes.get(at).copied() else {
            break;
        };
        if byte.is_ascii_whitespace() {
            if byte == b'\n' {
                line += 1;
            }
            at += 1;
            continue;
        }
        if bytes
            .get(at..)
            .is_some_and(|remaining| remaining.starts_with(b"//"))
        {
            while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
                at += 1;
            }
            continue;
        }
        if bytes
            .get(at..)
            .is_some_and(|remaining| remaining.starts_with(b"/*"))
        {
            at += 2;
            let mut depth = 1usize;
            while at < bytes.len() && depth > 0 {
                if bytes
                    .get(at..)
                    .is_some_and(|remaining| remaining.starts_with(b"/*"))
                {
                    depth += 1;
                    at += 2;
                } else if bytes
                    .get(at..)
                    .is_some_and(|remaining| remaining.starts_with(b"*/"))
                {
                    depth -= 1;
                    at += 2;
                } else {
                    if bytes.get(at) == Some(&b'\n') {
                        line += 1;
                    }
                    at += 1;
                }
            }
            continue;
        }
        if let Some(end) = char_literal_end(bytes, at) {
            at = end;
            continue;
        }
        if let Some((quote, raw, hashes)) = string_start(bytes, at) {
            let token_line = line;
            let content_start = quote + 1;
            let mut end = content_start;
            if raw {
                while let Some(byte) = bytes.get(end).copied() {
                    if byte == b'"'
                        && bytes
                            .get(end + 1..end + 1 + hashes)
                            .is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
                    {
                        break;
                    }
                    if byte == b'\n' {
                        line += 1;
                    }
                    end += 1;
                }
            } else {
                let mut escaped = false;
                while let Some(byte) = bytes.get(end).copied() {
                    if byte == b'\n' {
                        line += 1;
                    }
                    if byte == b'"' && !escaped {
                        break;
                    }
                    escaped = byte == b'\\' && !escaped;
                    end += 1;
                }
            }
            let value = String::from_utf8_lossy(bytes.get(content_start..end).unwrap_or_default())
                .into_owned();
            tokens.push(SourceToken {
                kind: SourceKind::String(value),
                line: token_line,
            });
            at = (end + 1 + if raw { hashes } else { 0 }).min(bytes.len());
            continue;
        }
        let token_line = line;
        let Some(byte) = bytes.get(at).copied() else {
            break;
        };
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                at += 1;
            }
            tokens.push(SourceToken {
                kind: SourceKind::Ident(
                    String::from_utf8_lossy(bytes.get(start..at).unwrap_or_default()).into_owned(),
                ),
                line: token_line,
            });
        } else {
            tokens.push(SourceToken {
                kind: SourceKind::Punct(byte as char),
                line: token_line,
            });
            at += 1;
        }
    }
    tokens
}

fn char_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'\'') {
        return None;
    }
    let mut end = at + 1;
    match bytes.get(end)? {
        b'\\' => {
            end += 1;
            match bytes.get(end)? {
                b'u' if bytes.get(end + 1) == Some(&b'{') => {
                    end += 2;
                    while bytes.get(end).is_some_and(|byte| *byte != b'}') {
                        end += 1;
                    }
                    end += 1;
                }
                b'x' => end += 3,
                _ => end += 1,
            }
        }
        b'\'' | b'\n' => return None,
        byte => {
            end += match byte {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return None,
            }
        }
    }
    (bytes.get(end) == Some(&b'\'')).then_some(end + 1)
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
        let mut quote = at + prefix.len();
        if prefix.ends_with(b"r") {
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                return Some((quote, true, quote - at - prefix.len()));
            }
        } else if bytes.get(quote) == Some(&b'"') {
            return Some((quote, false, 0));
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
    (start..end).any(|i| {
        ident(tokens.get(i), "cfg")
            && punct(tokens.get(i + 1), '(')
            && ident(tokens.get(i + 2), "test")
            && punct(tokens.get(i + 3), ')')
    })
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
            punct(tokens.get(*i), ';') || punct(tokens.get(*i), '{') || punct(tokens.get(*i), '}')
        })
        .map_or(0, |i| i + 1);
    (start..open).any(|i| {
        ["struct", "enum", "union"]
            .iter()
            .any(|name| ident(tokens.get(i), name))
    })
}

fn test_item_end(tokens: &[SourceToken], start: usize, field: bool) -> usize {
    let mut groups = Vec::new();
    for i in start..tokens.len() {
        if groups.is_empty() {
            if punct(tokens.get(i), ';') || (field && punct(tokens.get(i), ',')) {
                return i;
            }
            if field && punct(tokens.get(i), '}') {
                return i.saturating_sub(1);
            }
            if punct(tokens.get(i), '{') {
                return matching(tokens, i, '{', '}').unwrap_or(tokens.len() - 1);
            }
        }
        match tokens.get(i).map(|token| &token.kind) {
            Some(SourceKind::Punct('(')) => groups.push(')'),
            Some(SourceKind::Punct('[')) => groups.push(']'),
            Some(SourceKind::Punct('<')) if field => groups.push('>'),
            Some(SourceKind::Punct(close @ (')' | ']'))) if groups.last() == Some(close) => {
                groups.pop();
            }
            Some(SourceKind::Punct('>')) if groups.last() == Some(&'>') => {
                groups.pop();
            }
            _ => {}
        }
    }
    tokens.len().saturating_sub(1)
}

/// Marks cfg(test)-guarded items, including inline modules and file modules.
fn test_only_tokens(tokens: &[SourceToken]) -> Vec<bool> {
    let mut marked = vec![false; tokens.len()];
    if punct(tokens.first(), '#')
        && punct(tokens.get(1), '!')
        && punct(tokens.get(2), '[')
        && matching(tokens, 2, '[', ']').is_some_and(|end| cfg_test_attribute(tokens, 3, end))
    {
        marked.fill(true);
        return marked;
    }
    let mut i = 0;
    while i + 1 < tokens.len() {
        if !punct(tokens.get(i), '#') || !punct(tokens.get(i + 1), '[') {
            i += 1;
            continue;
        }
        let Some(attr_end) = matching(tokens, i + 1, '[', ']') else {
            i += 1;
            continue;
        };
        if !cfg_test_attribute(tokens, i + 2, attr_end) {
            i = attr_end + 1;
            continue;
        }
        let item = attr_end + 1;
        let end = test_item_end(tokens, item, struct_body(tokens, i));
        for marked_token in marked.iter_mut().take((end + 1).min(tokens.len())).skip(i) {
            *marked_token = true;
        }
        i = end + 1;
    }
    marked
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
        .map_or(0, |i| i + 1);
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

/// Production source files under crate `src/`, excluding conventional tests,
/// cfg(test) items and files referenced only from cfg(test) modules.
fn production_files(files: &[RustFile]) -> Vec<&RustFile> {
    let candidates: Vec<&RustFile> = files
        .iter()
        .filter(|f| f.krate != "fakes" && f.rel.starts_with("src/") && !is_test_file(&f.rel))
        .collect();
    let paths: BTreeSet<&str> = candidates.iter().map(|f| f.path.as_str()).collect();
    let mut edges = Vec::new();
    for file in &candidates {
        let tokens = source_tokens(&file.source);
        let tests = test_only_tokens(&tokens);
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
                edges.push((
                    file.path.as_str(),
                    target,
                    tests.get(i).copied().unwrap_or(false),
                ));
            }
        }
    }
    let mut test_only = BTreeSet::new();
    loop {
        let newly: Vec<String> = edges
            .iter()
            .filter(|(parent, _, test_edge)| *test_edge || test_only.contains(*parent))
            .map(|(_, target, _)| target.clone())
            .filter(|target| {
                !test_only.contains(target.as_str())
                    && !edges.iter().any(|(parent, incoming, is_test)| {
                        incoming == target && !is_test && !test_only.contains(*parent)
                    })
            })
            .collect();
        if newly.is_empty() {
            break;
        }
        test_only.extend(newly);
    }
    candidates
        .into_iter()
        .filter(|file| !test_only.contains(file.path.as_str()))
        .collect()
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
        tokens
            .get(range)
            .unwrap_or_default()
            .iter()
            .map(|token| match &token.kind {
                SourceKind::Ident(value) | SourceKind::String(value) => value.clone(),
                SourceKind::Punct(value) => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
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

fn joined_literal(tokens: &[SourceToken], at: usize) -> bool {
    at >= 3
        && punct(tokens.get(at - 3), '.')
        && ident(tokens.get(at - 2), "join")
        && punct(tokens.get(at - 1), '(')
}

fn home_join_context(tokens: &[SourceToken], join_dot: usize) -> Option<String> {
    let end = join_dot.checked_sub(1)?;
    if ident(tokens.get(end), "home") {
        let owner = if previous_punct(tokens, end, 1, '.') {
            match tokens.get(end.checked_sub(2)?) {
                Some(SourceToken {
                    kind: SourceKind::Ident(owner),
                    ..
                }) if matches!(owner.as_str(), "self" | "hub" | "inputs") => {
                    format!("{owner}.home")
                }
                _ => return None,
            }
        } else {
            "home".to_owned()
        };
        return Some(owner);
    }
    if !punct(tokens.get(end), ')') {
        return None;
    }
    let mut depth = 0usize;
    let mut open = None;
    for i in (0..=end).rev() {
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
    let open = open?;
    let previous_dot = open.checked_sub(2)?;
    if !ident(tokens.get(open.checked_sub(1)?), "join") || !punct(tokens.get(previous_dot), '.') {
        return None;
    }
    let base = home_join_context(tokens, previous_dot)?;
    let segment = string(tokens.get(open + 1))
        .or_else(|| match tokens.get(open + 1) {
            Some(SourceToken {
                kind: SourceKind::Ident(value),
                ..
            }) => Some(value.as_str()),
            _ => None,
        })
        .unwrap_or("*");
    Some(format!("{base}/{segment}"))
}

fn fiber_home_join(tokens: &[SourceToken], join_dot: usize) -> Option<String> {
    let context = home_join_context(tokens, join_dot)?;
    (!context.contains("/.agents") && !context.contains("/.config")).then_some(context)
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
    (at.saturating_sub(32)..at).any(|i| {
        operations.iter().any(|operation| {
            ident(tokens.get(i), operation)
                && punct(tokens.get(i + 1), '(')
                && matching(tokens, i + 1, '(', ')').is_some_and(|end| end >= at)
        })
    })
}

fn session_sites(file: &RustFile) -> Vec<SourceSite> {
    let tokens = source_tokens(&file.source);
    let test_only = test_only_tokens(&tokens);
    let mut sites = Vec::new();
    if file.krate == "log" {
        return sites;
    }
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(value) = string(Some(token)) else {
            continue;
        };
        let join = joined_literal(&tokens, i);
        let operation = if value == "sessions" && join {
            Some("join:sessions")
        } else if value == "session.lock" && join {
            Some("join:session.lock")
        } else if path_segment(value, "sessions") && (join || file_operation_near(&tokens, i)) {
            Some("path:sessions")
        } else if value == "events.jsonl" && (join || file_operation_near(&tokens, i)) {
            Some("join:events.jsonl")
        } else {
            None
        };
        if let Some(operation) = operation {
            sites.push(SourceSite {
                path: file.path.clone(),
                line: token.line,
                context: function_context(&tokens, i),
                operation: operation.to_owned(),
            });
        }
    }
    sites
}

fn prompt_literal(value: &str) -> bool {
    matches!(value, "SYSTEM.md" | "APPEND_SYSTEM.md")
}

fn home_sites(file: &RustFile) -> Vec<SourceSite> {
    let tokens = source_tokens(&file.source);
    let test_only = test_only_tokens(&tokens);
    let mut sites = Vec::new();
    if file.krate == "config" {
        return sites;
    }
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(value) = string(Some(token)) else {
            continue;
        };
        let join = joined_literal(&tokens, i);
        let home_context = if join {
            fiber_home_join(&tokens, i - 3)
        } else {
            None
        };
        let home_root = home_context.is_some();
        let first = value.split('/').next().unwrap_or_default();
        let home_component = HOME_COMPONENTS.contains(&first);
        let config_path = (first == ".fiber")
            && (value.split('/').nth(1) == Some("config.json")
                || value.split('/').nth(1) == Some("config"));
        let prompt = prompt_literal(value) && (home_root || file_operation_near(&tokens, i));
        let direct_config = config_path && (join || file_operation_near(&tokens, i));
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
                || function_context(&tokens, i),
                |root| format!("{}|root:{root}", function_context(&tokens, i)),
            );
            let context = home_scope_context(file, &tokens, i, context, &operation);
            sites.push(SourceSite {
                path: file.path.clone(),
                line: token.line,
                context,
                operation,
            });
        }
    }
    for i in 0..tokens.len().saturating_sub(4) {
        if !punct(tokens.get(i), '.')
            || !ident(tokens.get(i + 1), "join")
            || !punct(tokens.get(i + 2), '(')
            || !ident(tokens.get(i + 3), "name")
            || !punct(tokens.get(i + 4), ')')
            || test_only.get(i + 3).copied().unwrap_or(false)
            || function_context(&tokens, i + 3) != "fn read_one"
        {
            continue;
        }
        let Some(root) = fiber_home_join(&tokens, i) else {
            continue;
        };
        let Some(token) = tokens.get(i + 3) else {
            continue;
        };
        let operation = "join:prompt-name";
        let context = home_scope_context(
            file,
            &tokens,
            i,
            format!("fn read_one|root:{root}"),
            operation,
        );
        sites.push(SourceSite {
            path: file.path.clone(),
            line: token.line,
            context,
            operation: operation.to_owned(),
        });
    }
    sites
}

fn consume_next_allowance(
    remaining: &mut Vec<SourceAllowance>,
    sites: &mut impl Iterator<Item = SourceSite>,
) -> Option<&'static str> {
    let site = sites.next()?;
    let position = remaining.iter().position(|allowance| {
        allowance.path == site.path
            && allowance.context == site.context
            && allowance.operation == site.operation
    })?;
    Some(remaining.remove(position).ticket)
}

#[cfg(test)]
fn consume_allowance(allowances: &[SourceAllowance], site: &SourceSite) -> Option<&'static str> {
    let mut remaining = allowances.to_vec();
    consume_next_allowance(&mut remaining, &mut std::iter::once(site.clone()))
}

fn source_failures(files: &[RustFile], allowances: &[SourceAllowance], home: bool) -> Vec<String> {
    let mut remaining = allowances.to_vec();
    let mut failures = Vec::new();
    for file in production_files(files) {
        let sites = if home {
            home_sites(file)
        } else {
            session_sites(file)
        };
        for site in sites {
            if consume_next_allowance(&mut remaining, &mut std::iter::once(site.clone())).is_none()
            {
                let message = if home {
                    "Fiber-home path outside config"
                } else {
                    "session path outside log"
                };
                failures.push(format!("{}:{}: {message}", site.path, site.line));
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

fn derived_tool_names(files: &[RustFile]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in production_files(files)
        .into_iter()
        .filter(|file| matches!(file.krate.as_str(), "tools" | "jobs"))
    {
        let tokens = source_tokens(&file.source);
        let test_only = test_only_tokens(&tokens);
        for i in 0..tokens.len() {
            if test_only.get(i).copied().unwrap_or(false) {
                continue;
            }
            if let Some(name) = owned_tool_definition(&tokens, i) {
                names.insert(name);
            }
        }
    }
    names
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

fn loop_sites(file: &RustFile, names: &BTreeSet<String>) -> (Vec<SourceSite>, Vec<String>) {
    let tokens = source_tokens(&file.source);
    let test_only = test_only_tokens(&tokens);
    let mut sites = Vec::new();
    let mut failures = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if test_only.get(i).copied().unwrap_or(false) {
            continue;
        }
        if matches!(file.krate.as_str(), "tools" | "jobs")
            && unrecognized_tool_definition(&tokens, i)
        {
            failures.push(format!(
                "{}:{}: unrecognized owned tool-name definition",
                file.path, token.line
            ));
        }
        if file.krate != "loop" {
            continue;
        }
        let Some(name) = string(Some(token)).filter(|name| names.contains(*name)) else {
            continue;
        };
        let mut context = function_context(&tokens, i);
        if let Some((function, reason)) = label_context(file, &tokens, i, name) {
            let exception = LABEL_ALLOWANCES.iter().find(|allowance| {
                allowance.path == file.path
                    && allowance.function == function
                    && allowance.context
                        == match (file.path.as_str(), name) {
                            ("crates/loop/src/opening.rs", "shell") => "environment-template",
                            ("crates/loop/src/status.rs", "jobs") => "state-key",
                            ("crates/loop/src/skills.rs", "skill") => "command-tag",
                            ("crates/loop/src/handoff/carry.rs", "jobs") => "template-key",
                            ("crates/loop/src/reviewer.rs", "handoff") => "prompt-section",
                            _ => "",
                        }
                    && allowance.name == name
                    && allowance.reason == reason
            });
            if exception.is_some() {
                continue;
            }
            context = format!("{context} | {reason}");
        }
        sites.push(SourceSite {
            path: file.path.clone(),
            line: token.line,
            context,
            operation: format!("tool:{name}"),
        });
    }
    (sites, failures)
}

/// Built-in tool identities named by loop production source.
pub(crate) fn loop_tool_names(files: &[RustFile]) -> Vec<String> {
    let production = production_files(files);
    let names = derived_tool_names(files);
    let mut failures = Vec::new();
    for file in production {
        let (sites, definitions) = loop_sites(file, &names);
        failures.extend(definitions);
        for site in sites {
            failures.push(format!(
                "{}:{}: built-in tool name {}",
                site.path,
                site.line,
                site.operation.trim_start_matches("tool:")
            ));
        }
    }
    failures
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
