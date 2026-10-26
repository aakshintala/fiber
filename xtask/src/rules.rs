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
    source_allowance!("crates/jobs/src/delegate.rs", "fn start", "join:events.jsonl", "#1725"),
    source_allowance!("crates/main/src/resume.rs", "fn resumed_session", "join:events.jsonl", "#1725"),
    source_allowance!("crates/main/src/resume.rs", "fn is_fork_child", "join:events.jsonl", "#2507"),
    source_allowance!("crates/main/src/session_command.rs", "fn run_new", "join:events.jsonl", "#1725"),
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
