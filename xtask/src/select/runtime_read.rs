//! The run-time-read scanner: failures where a crate's Rust source
//! reads a repository file at run time instead of compiling it in.

mod joins;
mod paths;

use crate::rules::RustFile;
use crate::select::Members;
use joins::{
    format_probes, join_on_joined, join_on_root, joined_mention, rooted_format, second_mention,
};
pub(crate) use paths::string_literal;
use paths::{
    all_literals, all_lits, escapes, escapes_members, holds_root, holds_value, inline_lits,
    is_joined_name, is_manifest_rooted, is_path_like, is_receiver_boundary, is_scripts_path,
    joined_lit, resolve_in_root, root_name, root_returns, write_muted, yields_to_packages,
};
use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::iter::Peekable;

/// Failures where a crate's Rust source reads a repository file at run
/// time instead of compiling it in, one line each; empty when every read
/// is compiled in (`docs/ci.md`, "Selection"). A file has a run-time
/// read when its tokens hold, literals only, never comments:
///
/// - `read_to_string`, `read` or `open` directly given a path-like string
///   literal (holding `/` but no `:`) that resolves inside the repository
///   from the reading crate's manifest directory;
/// - `concat!` holding the literal `CARGO_MANIFEST_DIR` and a suffix
///   literal resolving inside the repository from the manifest directory;
/// - `format!` with all-literal arguments rooted at the manifest
///   directory, inside the argument of such a read, every suffix
///   resolving inside the repository;
/// - `join` given a literal naming a joined file, on a base rooted at
///   the manifest directory, inside the argument of such a read: the
///   receiver holds the manifest literal, or holds an identifier an
///   earlier `let` bound to the manifest directory itself, or holds an
///   identifier an earlier `let` bound to a join chain rooted there. A `let`
///   holding such a join reports its literals when a read takes the name,
///   through any number of `let`s. A join onto any other base, and an
///   absolute or escaping suffix, never report.
///
/// A helper that builds a repository path without reading it is a
/// run-time read too, reported outside any read: a manifest-rooted join
/// or format chain resolving outside every workspace member directory
/// and outside `scripts/`, unless it yields to `providers/` or
/// `extensions/`, names exactly the repository root, or sits inside an
/// include macro (`builds ...`); and a join threading a value onto a
/// bare manifest base, or onto a bound chain resolving outside every
/// member (`threads ...`). A binding a `write` or `create` call alone
/// consumes mutes its own chain, and a new-arm literal a read consumes
/// reports once with the reads-message.
///
/// A literal nested inside another call is a computed path, not a named
/// one: `read_to_string(home.join("docs/README.md"))` reads a temporary
/// directory. `read_dir` never reports: no include macro compiles a
/// directory in. A `providers/` or `extensions/` path reports only when
/// the file holds no manifest token: with the token the package-reader
/// check claims the read, keeping single ownership there. For crate
/// `xtask` only test files count (their `rel` ends `_tests.rs` or starts
/// `tests/`), because its commands read docs as tools. Err on a file that
/// does not tokenise.
pub(crate) fn runtime_read_mismatches(
    files: &[RustFile],
    members: &Members,
) -> Result<Vec<String>, String> {
    /// The mutable walk state: the bindings in scope, the reading crate's
    /// manifest directory, whether the file holds the manifest token, and
    /// the reported literals.
    struct Walk<'a> {
        root: &'a mut BTreeSet<String>,
        joined: &'a mut BTreeMap<String, Vec<String>>,
        second: &'a mut BTreeMap<String, (Vec<String>, Vec<String>)>,
        manifest_dir: &'a str,
        has_manifest: bool,
        members: &'a Members,
        found: &'a mut BTreeSet<String>,
        /// Write-only idents of the enclosing function, from the
        /// pre-scan: a binding mentioned only at its binding and in
        /// `write` or `create` first arguments mutes the new arms.
        muted: &'a mut BTreeSet<String>,
        /// Inside an `include_str!` or `include_bytes!` macro: the new
        /// arms never report include interiors.
        in_include: bool,
        /// Literals the escaping arm reports with the builds-message.
        escaping: &'a mut BTreeSet<String>,
        /// Names the threading arm reports with the threads-message.
        threading: &'a mut BTreeSet<String>,
        /// Every manifest-rooted chain a `let` binds, by name, for the
        /// post-pass that drops new-arm entries a read consumes.
        chains: &'a mut BTreeMap<String, Vec<String>>,
        /// Binding names a read consumes, for the same post-pass.
        read_used: &'a mut BTreeSet<String>,
    }
    // debt: format! in a read argument keeps its own windows(3) scan;
    // a second scanned macro moves both into the walk.
    /// The literals a `format!` in a read's argument group reports: only
    /// `format!` calls directly in the group count, so the thousands of
    /// `format!` calls elsewhere never report.
    fn format_literals(group: &proc_macro2::Group, cx: &Walk) -> Vec<String> {
        let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
        let mut out = Vec::new();
        for window in tokens.windows(3) {
            if let [
                TokenTree::Ident(name),
                TokenTree::Punct(mark),
                TokenTree::Group(argument),
            ] = window
                && name == "format"
                && mark.as_char() == '!'
                && let Some(literals) = rooted_format(argument, cx.manifest_dir, cx.has_manifest)
            {
                out.extend(literals);
            }
        }
        out
    }
    // debt: each read arm repeats the junk, marker, absolute, resolve and
    // yield checks by hand; a new arm centralizes them into one helper.
    /// The literals a read reports through a bound name, and the name:
    /// when the argument of a read is one name, directly or behind `&`
    /// with an optional trailing comma. A one-level join reports the
    /// literals resolving inside the repository; a deeper join reports
    /// its own literals, the full chain having been contained at binding.
    fn read_name(group: &proc_macro2::Group, cx: &Walk) -> Option<(String, Vec<String>)> {
        let mut tokens = group.stream().into_iter().peekable();
        while matches!(tokens.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '&') {
            tokens.next();
        }
        let name = match tokens.next() {
            Some(TokenTree::Ident(name)) => name,
            _ => return None,
        };
        match tokens.next() {
            None => {}
            Some(TokenTree::Punct(mark)) if mark.as_char() == ',' && tokens.next().is_none() => {}
            _ => return None,
        }
        if let Some((_, own)) = cx.second.get(&name.to_string()) {
            return Some((name.to_string(), own.clone()));
        }
        let literals = cx.joined.get(&name.to_string())?;
        let kept: Vec<String> = literals
            .iter()
            .filter(|literal| {
                is_joined_name(literal)
                    && *literal != "CARGO_MANIFEST_DIR"
                    // An absolute suffix replaces the receiver in a join.
                    && !literal.starts_with('/')
                    && resolve_in_root(cx.manifest_dir, std::slice::from_ref(literal))
                        .is_some_and(|resolved| !yields_to_packages(&resolved, cx.has_manifest))
            })
            .cloned()
            .collect();
        (!kept.is_empty()).then_some((name.to_string(), kept))
    }
    /// If `stream` holds a plain binding next (`mut`, a name, an
    /// optional `: <type>`, `=`), rebind the name to its value through
    /// the terminating `;`: a value joining onto the manifest directory
    /// binds the name to the joined literals; a value joining onto a
    /// joined name binds the name to the latest join's literals
    /// when the full chain stays inside the repository; a value holding
    /// the manifest literal and no `join` binds the name to the manifest
    /// directory itself; any other value unbinds the name. Walk the
    /// value either way. Anything else stays in the stream for the
    /// caller.
    fn bind(stream: &mut Peekable<std::vec::IntoIter<TokenTree>>, cx: &mut Walk, in_read: bool) {
        if matches!(stream.peek(), Some(TokenTree::Ident(name)) if name == "mut")
            && stream.next().is_none()
        {
            return;
        }
        let name = match stream.peek() {
            Some(TokenTree::Ident(name)) => name.to_string(),
            Some(TokenTree::Group(_) | TokenTree::Punct(_) | TokenTree::Literal(_)) | None => {
                return;
            }
        };
        if stream.next().is_none() {
            return;
        }
        if matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ':') {
            if stream.next().is_none() {
                return;
            }
            loop {
                match stream.peek() {
                    None => return,
                    Some(TokenTree::Punct(p)) if p.as_char() == '=' => break,
                    Some(TokenTree::Punct(p)) if p.as_char() == ';' => return,
                    Some(
                        TokenTree::Group(_)
                        | TokenTree::Ident(_)
                        | TokenTree::Punct(_)
                        | TokenTree::Literal(_),
                    ) => {}
                }
                if stream.next().is_none() {
                    return;
                }
            }
        }
        if !matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '=') {
            return;
        }
        if stream.next().is_none() {
            return;
        }
        let mut rhs = Vec::new();
        for tree in stream.by_ref() {
            if let TokenTree::Punct(punct) = &tree
                && punct.as_char() == ';'
            {
                break;
            }
            rhs.push(tree);
        }
        // Walk the value with the old bindings first: Rust resolves a
        // shadowing `let`'s value to the previous bindings, so the
        // value must report through them before the new one replaces
        // them.
        let shadowed = join_on_root(&rhs, &*cx.root);
        let chained = if shadowed.is_none() {
            join_on_joined(&rhs, &*cx.joined, &*cx.second)
        } else {
            None
        };
        let rooted = is_manifest_rooted(&rhs)
            && !rhs
                .iter()
                .any(|tree| matches!(tree, TokenTree::Ident(join) if join == "join"));
        walk(rhs, cx, in_read);
        if let Some(literals) = shadowed {
            cx.root.remove(name.as_str());
            cx.second.remove(name.as_str());
            cx.joined.insert(name.clone(), literals.clone());
            cx.chains.insert(name.clone(), literals.clone());
            if cx.muted.contains(name.as_str()) {
                for literal in &literals {
                    cx.escaping.remove(literal);
                }
            }
        } else if let Some((chain, own)) = chained {
            // The full chain must resolve inside the repository and stay
            // out of the package reader's paths: the report names only
            // the latest join's literals, which no longer carry the base.
            let contained = resolve_in_root(cx.manifest_dir, &chain)
                .is_some_and(|resolved| !yields_to_packages(&resolved, cx.has_manifest));
            cx.root.remove(name.as_str());
            cx.joined.remove(name.as_str());
            if contained {
                cx.second.insert(name.clone(), (chain.clone(), own.clone()));
                cx.chains.insert(name.clone(), chain);
                if cx.muted.contains(name.as_str()) {
                    for literal in &own {
                        cx.escaping.remove(literal);
                    }
                }
            } else {
                cx.second.remove(name.as_str());
                cx.chains.remove(name.as_str());
            }
        } else if rooted {
            cx.joined.remove(name.as_str());
            cx.second.remove(name.as_str());
            cx.root.insert(name.clone());
            cx.chains.insert(name, Vec::new());
        } else {
            cx.root.remove(name.as_str());
            cx.joined.remove(name.as_str());
            cx.second.remove(name.as_str());
            cx.chains.remove(name.as_str());
        }
    }
    /// Walk a group's tokens with the bindings scoped to it: bindings
    /// made inside do not leak out, and an inner `let` never changes
    /// the outer set. The one place groups enter a fresh scope.
    fn walk_group(group: &proc_macro2::Group, cx: &mut Walk, in_read: bool) {
        let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
        walk(
            tokens,
            &mut Walk {
                root: &mut cx.root.clone(),
                joined: &mut cx.joined.clone(),
                second: &mut cx.second.clone(),
                manifest_dir: cx.manifest_dir,
                has_manifest: cx.has_manifest,
                members: cx.members,
                found: &mut *cx.found,
                muted: &mut *cx.muted,
                in_include: cx.in_include,
                escaping: &mut *cx.escaping,
                threading: &mut *cx.threading,
                chains: &mut *cx.chains,
                read_used: &mut *cx.read_used,
            },
            in_read,
        );
    }
    fn walk(tokens: Vec<TokenTree>, cx: &mut Walk, in_read: bool) {
        let mut stream = tokens.into_iter().peekable();
        // The current expression back to its nearest boundary: the
        // receiver a `join` builds on.
        let mut preceding: Vec<TokenTree> = Vec::new();
        // Groups seen since the `fn` head started: the second one is
        // the body, whose pre-scan mutes write-only idents.
        let mut fn_head: Option<u8> = None;
        loop {
            let Some(tree) = stream.next() else {
                break;
            };
            match tree {
                TokenTree::Punct(punct) if is_receiver_boundary(punct.as_char()) => {
                    preceding.clear();
                    fn_head = None;
                }
                TokenTree::Ident(ident) if ident == "let" => {
                    bind(&mut stream, cx, in_read);
                    preceding.clear();
                }
                TokenTree::Ident(ident) if ident == "fn" => {
                    fn_head = Some(0);
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident)
                    if ident == "read_to_string" || ident == "read" || ident == "open" =>
                {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        for tree in group.stream() {
                            if let TokenTree::Literal(lit) = tree
                                && let Some(text) = string_literal(&lit.to_string())
                                && is_joined_name(&text)
                                && is_path_like(&text)
                                // An absolute path names no repository file:
                                // the read takes no base.
                                && !text.starts_with('/')
                                && let Some(resolved) =
                                    resolve_in_root(cx.manifest_dir, std::slice::from_ref(&text))
                                && !yields_to_packages(&resolved, cx.has_manifest)
                            {
                                cx.found.insert(text);
                            }
                        }
                        if let Some((name, literals)) = read_name(&group, cx) {
                            cx.read_used.insert(name);
                            cx.found.extend(literals);
                        }
                        cx.found.extend(format_literals(&group, cx));
                        walk_group(&group, cx, true);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "read_dir" => {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        // Directory reads never report: no include macro
                        // compiles a directory in.
                        walk_group(&group, cx, false);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "include_str" || ident == "include_bytes" => {
                    if matches!(stream.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '!')
                    {
                        stream.next();
                        match stream.next() {
                            Some(TokenTree::Group(group)) => {
                                let held = cx.in_include;
                                cx.in_include = true;
                                walk_group(&group, cx, in_read);
                                cx.in_include = held;
                                preceding.push(TokenTree::Group(group));
                            }
                            Some(other) => {
                                // `!` without a group is no macro call:
                                // replay the token through the walk.
                                walk(vec![other], cx, in_read);
                            }
                            None => {}
                        }
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "format" => {
                    if matches!(stream.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '!')
                        && stream.next().is_some()
                        && matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        // Outside any read the old arms stay silent, so
                        // the escaping arm reports the suffixes; inside a
                        // read the old arm owns the template.
                        if !in_read
                            && !cx.in_include
                            && let Some(probes) = format_probes(&group)
                        {
                            for probe in probes {
                                if let Some(resolved) =
                                    resolve_in_root(cx.manifest_dir, std::slice::from_ref(&probe))
                                    && escapes(&resolved, cx.members, cx.has_manifest)
                                {
                                    cx.escaping.insert(probe);
                                }
                            }
                        }
                        walk_group(&group, cx, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "concat" => {
                    if matches!(stream.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                        && stream.next().is_some()
                        && matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        let mut literals = Vec::new();
                        all_literals(group.stream(), &mut literals);
                        if literals
                            .iter()
                            .any(|literal| literal == "CARGO_MANIFEST_DIR")
                        {
                            for literal in literals {
                                if literal == "CARGO_MANIFEST_DIR"
                                    || !is_joined_name(&literal)
                                    // A bare word is a computed value, not a
                                    // suffix: the block test pins `"x"`.
                                    || !is_path_like(&literal)
                                {
                                    continue;
                                }
                                if let Some(resolved) =
                                    resolve_in_root(cx.manifest_dir, std::slice::from_ref(&literal))
                                    && !yields_to_packages(&resolved, cx.has_manifest)
                                {
                                    cx.found.insert(literal);
                                }
                            }
                        }
                        walk_group(&group, cx, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Ident(ident) if ident == "join" => {
                    if matches!(stream.peek(), Some(TokenTree::Group(_)))
                        && let Some(TokenTree::Group(group)) = stream.next()
                    {
                        if in_read {
                            let base: Option<Vec<String>> = if is_manifest_rooted(&preceding)
                                || holds_root(&preceding, &*cx.root)
                            {
                                Some(Vec::new())
                            } else if let Some(name) = joined_mention(&preceding, &*cx.joined) {
                                cx.joined.get(&name).cloned()
                            } else {
                                second_mention(&preceding, &*cx.second).and_then(|name| {
                                    cx.second.get(&name).map(|(full, _)| full.clone())
                                })
                            };
                            if let Some(chain) = base {
                                let mut own = Vec::new();
                                for tree in group.stream() {
                                    if let TokenTree::Literal(lit) = tree
                                        && let Some(text) = string_literal(&lit.to_string())
                                        && joined_lit(&text)
                                    {
                                        own.push(text);
                                    }
                                }
                                let mut full = chain;
                                full.extend(own.clone());
                                if !own.is_empty()
                                    && let Some(resolved) = resolve_in_root(cx.manifest_dir, &full)
                                    && !yields_to_packages(&resolved, cx.has_manifest)
                                {
                                    cx.found.extend(own);
                                }
                            }
                            if let Some(name) = joined_mention(&preceding, &*cx.joined)
                                .or_else(|| second_mention(&preceding, &*cx.second))
                                .or_else(|| root_name(&preceding, &*cx.root))
                            {
                                cx.read_used.insert(name);
                            }
                        }
                        if !cx.in_include {
                            // The escaping arm: literal suffixes on a
                            // manifest-rooted base outside every read. A
                            // read's own argument stays with the old arm.
                            let mut own = Vec::new();
                            for tree in group.stream() {
                                if let TokenTree::Literal(lit) = tree
                                    && let Some(text) = string_literal(&lit.to_string())
                                    && joined_lit(&text)
                                {
                                    own.push(text);
                                }
                            }
                            // The base the join builds on: inline manifest
                            // literals, or a bound name's chain with the
                            // inline receivers included.
                            let base: Option<(Vec<String>, Option<String>)> =
                                if is_manifest_rooted(&preceding) {
                                    Some((inline_lits(&preceding), None))
                                } else if let Some(found) = joined_mention(&preceding, &*cx.joined)
                                {
                                    let mut chain =
                                        cx.joined.get(&found).cloned().unwrap_or_default();
                                    chain.extend(all_lits(&preceding));
                                    Some((chain, Some(found)))
                                } else if let Some(found) = second_mention(&preceding, &*cx.second)
                                {
                                    let mut chain = cx
                                        .second
                                        .get(&found)
                                        .map(|(full, _)| full.clone())
                                        .unwrap_or_default();
                                    chain.extend(all_lits(&preceding));
                                    Some((chain, Some(found)))
                                } else {
                                    root_name(&preceding, &*cx.root)
                                        .map(|found| (all_lits(&preceding), Some(found)))
                                };
                            if let Some((chain, name)) = base {
                                let muted_base = name
                                    .as_deref()
                                    .is_some_and(|bound| cx.muted.contains(bound));
                                if !muted_base {
                                    let mut full = chain.clone();
                                    full.extend(own.clone());
                                    if !in_read
                                        && !own.is_empty()
                                        && let Some(resolved) =
                                            resolve_in_root(cx.manifest_dir, &full)
                                        && escapes(&resolved, cx.members, cx.has_manifest)
                                    {
                                        cx.escaping.extend(own.clone());
                                    }
                                    if holds_value(&group) {
                                        match (chain.is_empty(), name) {
                                            // A bare inline base reports
                                            // whatever member it sits in.
                                            (true, None) => {
                                                cx.threading
                                                    .insert("CARGO_MANIFEST_DIR".to_owned());
                                            }
                                            // A bare bound base names its
                                            // identifier.
                                            (true, Some(bound)) => {
                                                cx.threading.insert(bound);
                                            }
                                            // A bound chain reports only
                                            // outside every member, unless
                                            // the escaping arm already
                                            // covers its literals.
                                            (false, Some(bound)) => {
                                                if let Some(resolved) =
                                                    resolve_in_root(cx.manifest_dir, &full)
                                                    && escapes_members(&resolved, cx.members)
                                                    && !is_scripts_path(&resolved)
                                                    && !yields_to_packages(
                                                        &resolved,
                                                        cx.has_manifest,
                                                    )
                                                    && !cx
                                                        .escaping
                                                        .iter()
                                                        .any(|lit| full.contains(lit))
                                                {
                                                    cx.threading.insert(bound);
                                                }
                                            }
                                            // An inline chain never
                                            // threads: the escaping arm
                                            // owns its literals.
                                            (false, None) => {}
                                        }
                                    }
                                }
                            }
                        }
                        walk_group(&group, cx, in_read);
                        preceding.push(TokenTree::Group(group));
                    }
                    preceding.push(TokenTree::Ident(ident));
                }
                TokenTree::Group(group) => {
                    if fn_head == Some(1) {
                        // The function body: mute its write-only idents
                        // and report a bare root it returns.
                        fn_head = None;
                        let body: Vec<TokenTree> = group.stream().into_iter().collect();
                        let previous = std::mem::take(&mut *cx.muted);
                        *cx.muted = write_muted(&body);
                        cx.escaping
                            .extend(root_returns(&body, cx.manifest_dir, &*cx.root));
                        walk_group(&group, cx, in_read);
                        *cx.muted = previous;
                    } else {
                        if fn_head == Some(0) {
                            fn_head = Some(1);
                        }
                        walk_group(&group, cx, in_read);
                    }
                    preceding.push(TokenTree::Group(group));
                }
                TokenTree::Ident(ident) => preceding.push(TokenTree::Ident(ident)),
                TokenTree::Punct(punct) => preceding.push(TokenTree::Punct(punct)),
                TokenTree::Literal(lit) => preceding.push(TokenTree::Literal(lit)),
            }
        }
    }
    let mut failures = Vec::new();
    for f in files {
        if !members.contains_key(&f.krate) {
            continue;
        }
        if f.krate == "xtask" && !(f.rel.ends_with("_tests.rs") || f.rel.starts_with("tests/")) {
            continue;
        }
        let source: TokenStream = f
            .source
            .parse()
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?;
        let member = &members[&f.krate];
        let mut manifest_literals = Vec::new();
        all_literals(source.clone(), &mut manifest_literals);
        let has_manifest = manifest_literals
            .iter()
            .any(|literal| literal == "CARGO_MANIFEST_DIR");
        let mut root = BTreeSet::new();
        let mut joined = BTreeMap::new();
        let mut second = BTreeMap::new();
        let mut found = BTreeSet::new();
        let mut muted = BTreeSet::new();
        let mut escaping = BTreeSet::new();
        let mut threading = BTreeSet::new();
        let mut chains = BTreeMap::new();
        let mut read_used = BTreeSet::new();
        walk(
            source.into_iter().collect(),
            &mut Walk {
                root: &mut root,
                joined: &mut joined,
                second: &mut second,
                manifest_dir: &member.dir,
                has_manifest,
                members,
                found: &mut found,
                muted: &mut muted,
                in_include: false,
                escaping: &mut escaping,
                threading: &mut threading,
                chains: &mut chains,
                read_used: &mut read_used,
            },
            false,
        );
        // The old arms own every literal they report: a new-arm entry
        // for the same literal drops, so a bound file reaching a read
        // reports once with the reads-message.
        escaping.retain(|literal| !found.contains(literal));
        // A new-arm entry belonging to the manifest-rooted chain of a
        // binding a read consumes drops the same way.
        escaping.retain(|literal| {
            !read_used.iter().any(|name| {
                chains
                    .get(name)
                    .is_some_and(|chain| chain.contains(literal))
            })
        });
        failures.extend(found.into_iter().map(|literal| {
            format!(
                "{}: reads {literal} at run time; compile it in with include_str! or include_bytes!",
                f.path
            )
        }));
        failures.extend(escaping.into_iter().map(|literal| {
            format!(
                "{}: builds {literal} at run time; compile it in with include_str! or include_bytes!",
                f.path
            )
        }));
        failures.extend(threading.into_iter().map(|name| {
            format!(
                "{}: threads {name} into a path at run time; compile it in with include_str! or include_bytes!",
                f.path
            )
        }));
    }
    Ok(failures)
}

#[cfg(test)]
#[path = "runtime_read_tests.rs"]
mod tests;
