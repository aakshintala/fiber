//! The read-only recogniser: a tree-sitter-bash parse of one shell call,
//! kept only when every construct in it is on the allow-list below.
//!
//! The grammar names the shapes; this module accepts a small subset: a
//! program of commands joined by `;`, `&&`, `||` or `|`, where each command
//! is leading assignments, one name, plain words and the three redirects
//! `>/dev/null`, `>&1`-style merges and `<file` inputs. Anything else is an
//! opaque stage (the call still classifies, but never reads) or, for a broken
//! tree or a statement separator the recogniser does not implement, no
//! script at all (the call classifies with no subject).
//!
//! Allowed named kinds: program, command, command_name, variable_assignment,
//! variable_name, list, pipeline, redirected_statement, file_redirect,
//! file_descriptor, word, number, string, string_content, raw_string,
//! concatenation, command_substitution. Allowed tokens: `;`, `&&`, `||`,
//! `|`, `|&` (refused, but
//! recognised), `=`, `>`, `>&`, `<`. A `|&` pipe, `&`, a newline between
//! statements, or any other token or kind, refuses the stage or the call.

use tree_sitter::Node;

/// The longest command the recogniser parses; longer calls classify with no
/// subject. The bound keeps one classification cheap: the tree walk below is
/// iterative, so even a 10,000-byte `ls&&ls&&...` list cannot overflow the
/// stack it runs on.
pub(super) const MAX_COMMAND_BYTES: usize = 10_000;

/// One parsed call: its `&&`/`||`/`;` elements in source order.
/// Each element remembers the join that runs it: the first element is
/// `First`, and the rest follow `&&`, `||` or `;`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Join {
    First,
    And,
    Or,
    Seq,
}

/// One parsed call: its elements in source order.
pub(super) struct Script {
    pub(super) elements: Vec<Element>,
}

/// One `&&`/`||`/`;` element: its `|` pipeline stages in source order,
/// and the join that runs it.
pub(super) struct Element {
    pub(super) stages: Vec<Stage>,
    pub(super) join: Join,
}

/// One pipeline stage: a simple command the recogniser read fully, or an
/// opaque statement (a subshell, a loop, a refused redirect) that still
/// classifies but never reads. The payload is the stage's verbatim source
/// slice, so the review note names the stage as written.
pub(super) enum Stage {
    Simple(Simple),
    Opaque(String),
}

/// One simple command: its leading `NAME=value` assignments, its operand
/// words after quote removal, its redirects in source order, and the tokens
/// the subject is built from: the operand words and redirect spellings in
/// source order, assignments excluded. `text` is the stage's verbatim
/// source slice, redirects included, so the review note names the stage as
/// written; `start` and `end` are its byte span, extended as outer
/// redirects attach.
pub(super) struct Simple {
    pub(super) assignments: Vec<(String, String)>,
    pub(super) words: Vec<Word>,
    pub(super) redirects: Vec<Redirect>,
    pub(super) tokens: Vec<String>,
    pub(super) text: String,
    start: usize,
    end: usize,
}

/// One operand word: its raw spelling and its cooked text after quote
/// removal. A quoted spelling keeps its quotes in raw, so it never counts
/// as a redirect descriptor no matter its text. `substituted` marks a word
/// holding a `$(` substitution: the classifier validates its text, and
/// only a validated operand of a listed read keeps the call read-only.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Word {
    pub(super) raw: String,
    pub(super) cooked: String,
    pub(super) substituted: bool,
}

/// The three redirects the recogniser accepts: output discarded to
/// `/dev/null`, one stream merged into descriptor 1 or 2, and input read
/// from a file. The destination word of an input is checked like an operand.
pub(super) enum Redirect {
    Discard,
    Merge,
    Input(Word),
}

/// Parse `command` into a script, or nothing when the call is too long, holds
/// a NUL byte, fails to parse, or joins its statements with a separator the
/// recogniser does not implement.
pub(super) fn parse(command: &str) -> Option<Script> {
    if command.len() > MAX_COMMAND_BYTES || command.contains('\0') {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(command, None)?;
    let root = tree.root_node();
    if root.kind() != "program" || root.has_error() {
        return None;
    }
    let mut elements = Vec::new();
    let mut cursor = root.walk();
    let mut children = root.children(&mut cursor);
    let mut next = children.next();
    let mut join = Join::First;
    loop {
        let statement = next?;
        if statement.kind() == "comment" {
            return None;
        }
        push_statement(statement, command, &mut elements, join);
        let Some(separator) = children.next() else {
            break;
        };
        if separator.kind() != ";" {
            return None;
        }
        join = Join::Seq;
        next = children.next();
    }
    if elements.is_empty() {
        return None;
    }
    Some(Script { elements })
}

/// Convert one program-level statement into elements. Every shape lands here;
/// anything the recogniser does not implement becomes one opaque stage, so
/// conversion itself never fails.
fn push_statement(node: Node, src: &str, elements: &mut Vec<Element>, join: Join) {
    match node.kind() {
        "command" => elements.push(Element {
            stages: vec![
                cook_command(node, src)
                    .map_or_else(|| Stage::Opaque(text_of(node, src)), Stage::Simple),
            ],
            join,
        }),
        "list" => push_list(node, src, elements, join),
        "pipeline" => elements.push(pipeline(node, src, join)),
        "redirected_statement" => elements.extend(redirected(node, src, join)),
        "variable_assignment" => elements.push(Element {
            stages: vec![cook_assignment(node, src).map_or_else(
                || Stage::Opaque(text_of(node, src)),
                |assignment| Stage::Simple(Simple::assigned(assignment, node, src)),
            )],
            join,
        }),
        _ => elements.push(Element {
            stages: vec![Stage::Opaque(text_of(node, src))],
            join,
        }),
    }
}

/// Flatten a left-nested `&&`/`||` list into elements in source order, with
/// an explicit stack instead of recursion.
fn push_list(node: Node, src: &str, elements: &mut Vec<Element>, incoming: Join) {
    let mut stack = vec![(node, incoming)];
    while let Some((node, join)) = stack.pop() {
        if node.kind() != "list" {
            push_statement(node, src, elements, join);
            continue;
        }
        let mut cursor = node.walk();
        let mut children = node.children(&mut cursor);
        let (Some(left), Some(operator), Some(right)) =
            (children.next(), children.next(), children.next())
        else {
            elements.push(Element {
                stages: vec![Stage::Opaque(text_of(node, src))],
                join,
            });
            continue;
        };
        let operator_join = match operator.kind() {
            "&&" => Join::And,
            "||" => Join::Or,
            _ => {
                elements.push(Element {
                    stages: vec![Stage::Opaque(text_of(node, src))],
                    join,
                });
                continue;
            }
        };
        if children.next().is_some() {
            elements.push(Element {
                stages: vec![Stage::Opaque(text_of(node, src))],
                join,
            });
            continue;
        }
        stack.push((right, operator_join));
        stack.push((left, join));
    }
}

/// Convert one `|` pipeline into a single element. A `|&` pipe makes the
/// whole element opaque; any other surprise does the same.
fn pipeline(node: Node, src: &str, join: Join) -> Element {
    let opaque = || Element {
        stages: vec![Stage::Opaque(text_of(node, src))],
        join,
    };
    let mut stages = Vec::new();
    let mut cursor = node.walk();
    let mut children = node.children(&mut cursor);
    let mut next = children.next();
    loop {
        let Some(stage) = next else {
            return opaque();
        };
        match stage.kind() {
            "command" => {
                stages.push(
                    cook_command(stage, src)
                        .map_or_else(|| Stage::Opaque(text_of(stage, src)), Stage::Simple),
                );
            }
            "redirected_statement" => {
                let Some(stage) = single_stage(redirected(stage, src, join)) else {
                    return opaque();
                };
                stages.push(stage);
            }
            "variable_assignment" => stages.push(cook_assignment(stage, src).map_or_else(
                || Stage::Opaque(text_of(stage, src)),
                |assignment| Stage::Simple(Simple::assigned(assignment, stage, src)),
            )),
            _ => return opaque(),
        }
        let Some(separator) = children.next() else {
            break;
        };
        if separator.kind() == "|&" {
            return opaque();
        }
        if separator.kind() != "|" {
            return opaque();
        }
        next = children.next();
    }
    Element { stages, join }
}

/// The one stage of a redirect body standing alone, if it does: exactly
/// one element holding exactly one stage.
fn single_stage(nested: Vec<Element>) -> Option<Stage> {
    let mut nested = nested.into_iter();
    let only = nested.next()?;
    if nested.next().is_some() {
        return None;
    }
    let mut stages = only.stages.into_iter();
    let first = stages.next()?;
    if stages.next().is_some() {
        return None;
    }
    Some(first)
}

/// Convert a redirected statement: its body plus each redirect, the leading
/// ones onto the first stage and the trailing ones onto the last. A refused
/// redirect opaques its stage; anything but one body opaques the call.
fn redirected(node: Node, src: &str, incoming: Join) -> Vec<Element> {
    let opaque = || {
        vec![Element {
            stages: vec![Stage::Opaque(text_of(node, src))],
            join: incoming,
        }]
    };
    let mut body = None;
    let mut leading = Vec::new();
    let mut trailing = Vec::new();
    for child in node.children(&mut node.walk()) {
        match child.kind() {
            "command" | "list" | "pipeline" => {
                if body.is_some() {
                    return opaque();
                }
                body = Some(child);
            }
            "file_redirect" => {
                // Children arrive in source order, so a redirect before
                // the body has none yet; comparing start bytes says the
                // same and leaves two mutants with nothing to flip.
                if body.is_none() {
                    leading.push(child);
                } else {
                    trailing.push(child);
                }
            }
            _ => return opaque(),
        }
    }
    let Some(body) = body else {
        return opaque();
    };
    let mut elements = Vec::new();
    push_statement(body, src, &mut elements, incoming);
    for redirect in leading.iter().rev() {
        attach_simple(&mut elements, *redirect, src, false);
    }
    for redirect in trailing {
        attach_simple(&mut elements, redirect, src, true);
    }
    elements
}

/// Attach one redirect to the first stage of the first element (`trailing`
/// false) or the last stage of the last element (`trailing` true). A refused
/// redirect opaques its stage instead.
fn attach_simple(elements: &mut [Element], redirect: Node, src: &str, trailing: bool) {
    let element = if trailing {
        elements.last_mut()
    } else {
        elements.first_mut()
    };
    let Some(element) = element else {
        return;
    };
    let stage = if trailing {
        element.stages.last_mut()
    } else {
        element.stages.first_mut()
    };
    let Some(stage) = stage else {
        return;
    };
    let refused = match stage {
        Stage::Simple(simple) => {
            if push_redirect(simple, redirect, src, !trailing) {
                extend_text(simple, redirect, src);
                None
            } else {
                Some(merged_text(simple, redirect, src))
            }
        }
        Stage::Opaque(_) => None,
    };
    if let Some(text) = refused {
        *stage = Stage::Opaque(text);
    }
}

/// The source slice covering the stage's span and the redirect's, with
/// its bounds, or the stage's own slice and span when the union does not
/// slice the command (unreachable for nodes the parser returned).
fn covering_text(simple: &Simple, redirect: Node, src: &str) -> (String, usize, usize) {
    let start = simple.start.min(redirect.start_byte());
    let end = simple.end.max(redirect.end_byte());
    match src.get(start..end) {
        Some(text) => (text.to_string(), start, end),
        None => (simple.text.clone(), simple.start, simple.end),
    }
}

/// Widen a stage's span over an attached outer redirect, keeping the
/// verbatim source slice the review note names.
fn extend_text(simple: &mut Simple, redirect: Node, src: &str) {
    let (text, start, end) = covering_text(simple, redirect, src);
    simple.text = text;
    simple.start = start;
    simple.end = end;
}

/// The verbatim source slice of a refused stage with its refused redirect.
fn merged_text(simple: &Simple, redirect: Node, src: &str) -> String {
    covering_text(simple, redirect, src).0
}

/// The verbatim source slice of `node`, or empty when its span does not
/// slice the command (unreachable for a node the parser returned).
fn text_of(node: Node, src: &str) -> String {
    text(node, src).unwrap_or_default()
}

/// Build one simple command from a `command` node: leading assignments, one
/// name, plain words, and redirects in source order. Gaps between children
/// hold only blanks; anything else (a carriage return splitting two words)
/// refuses the stage, as does anything the recogniser does not implement.
fn cook_command(node: Node, src: &str) -> Option<Simple> {
    let mut simple = Simple::empty(node, src);
    let mut seen_name = false;
    let mut end = node.start_byte();
    for child in node.children(&mut node.walk()) {
        if !gap_is_blank(src, end, child.start_byte()) {
            return None;
        }
        end = child.end_byte();
        match child.kind() {
            "variable_assignment" => {
                if seen_name {
                    return None;
                }
                let (name, value) = cook_assignment(child, src)?;
                simple.assignments.push((name, value));
            }
            "command_name" => {
                if seen_name {
                    return None;
                }
                seen_name = true;
                let word = cook_name(child, src)?;
                simple.tokens.push(word.raw.clone());
                simple.words.push(word);
            }
            "word"
            | "number"
            | "string"
            | "raw_string"
            | "concatenation"
            | "command_substitution" => {
                seen_name = true;
                let word = cook_word(child, src)?;
                simple.tokens.push(word.raw.clone());
                simple.words.push(word);
            }
            "file_redirect" => {
                if !push_redirect(&mut simple, child, src, false) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(simple)
}

/// Read one `NAME=value` node into its name and cooked value. A missing
/// value cooks to the empty string; an unreadable value refuses the stage.
fn cook_assignment(node: Node, src: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut value = None;
    for child in node.children(&mut node.walk()) {
        match child.kind() {
            "variable_name" => {
                if name.is_some() {
                    return None;
                }
                name = text(child, src);
            }
            "word" | "number" | "string" | "raw_string" | "concatenation" => {
                if value.is_some() {
                    return None;
                }
                let word = cook_word(child, src)?;
                if word.substituted {
                    return None;
                }
                value = Some(word.cooked);
            }
            "=" => {}
            _ => return None,
        }
    }
    // The grammar's `variable_name` is `\w+`, so the name is never empty
    // and holds only ASCII alphanumerics and `_`; a subscript names a
    // different node kind and never reaches this filter.
    let name = name?;
    Some((name, value.unwrap_or_default()))
}

/// Validate one redirect and push it onto the stage: a bare all-digit word
/// right before a descriptor-less redirect is its descriptor, the way the
/// shell reads `0<f` but keeps the quoted `"2"` in `"2">/dev/null` an
/// operand. Anything outside the allow-list refuses the stage.
fn push_redirect(simple: &mut Simple, node: Node, src: &str, leading: bool) -> bool {
    let mut descriptor = None;
    let mut operator = None;
    let mut destinations = Vec::new();
    for child in node.children(&mut node.walk()) {
        match child.kind() {
            "file_descriptor" => {
                if descriptor.is_some() {
                    return false;
                }
                let Some(text) = text(child, src) else {
                    return false;
                };
                if text.bytes().any(|b| !b.is_ascii_digit()) {
                    return false;
                }
                descriptor = Some(text);
            }
            "word" | "number" | "string" | "raw_string" | "concatenation" => {
                let Some(word) = cook_word(child, src) else {
                    return false;
                };
                if word.substituted {
                    return false;
                }
                destinations.push((word, child.end_byte()));
            }
            ">" | ">&" | "<" => {
                if operator.is_some() {
                    return false;
                }
                operator = Some(child.kind());
            }
            _ => return false,
        }
    }
    let Some(operator) = operator else {
        return false;
    };
    // The grammar attaches words after the destination to the redirect,
    // as in `git 2>&1 status`: the first is the target, the rest are
    // command operands the redirect stands among.
    let mut destinations = destinations.into_iter();
    let Some((destination, dst_end)) = destinations.next() else {
        return false;
    };
    let extras: Vec<(Word, usize)> = destinations.collect();
    if descriptor.is_none() {
        descriptor = steal_descriptor(simple, node, src);
    }
    let redirect = match operator {
        ">" => {
            if !matches!(descriptor.as_deref(), None | Some("1") | Some("2")) {
                return false;
            }
            if destination.cooked != "/dev/null" {
                return false;
            }
            Redirect::Discard
        }
        ">&" => {
            if !matches!(descriptor.as_deref(), None | Some("1") | Some("2")) {
                return false;
            }
            if destination.cooked != "1" && destination.cooked != "2" {
                return false;
            }
            Redirect::Merge
        }
        "<" => {
            if !matches!(descriptor.as_deref(), None | Some("0")) {
                return false;
            }
            if destination.cooked.starts_with("/dev/tcp/")
                || destination.cooked.starts_with("/dev/udp/")
            {
                return false;
            }
            Redirect::Input(destination)
        }
        _ => return false,
    };
    let Some(raw) = src.get(node.start_byte()..dst_end).map(str::to_string) else {
        return false;
    };
    simple.redirects.push(redirect);
    let mut spellings = vec![raw];
    for (extra, _) in extras {
        spellings.push(extra.raw.clone());
        simple.words.push(Word {
            raw: extra.raw.clone(),
            cooked: extra.cooked.clone(),
            substituted: extra.substituted,
        });
    }
    if leading {
        for spelling in spellings.into_iter().rev() {
            simple.tokens.insert(0, spelling);
        }
    } else {
        simple.tokens.extend(spellings);
    }
    true
}

/// Take the last word as a redirect's descriptor when it is a bare all-digit
/// word spelled right against the redirect, and drop it from the words and
/// tokens. Quoted spellings stay operands.
fn steal_descriptor(simple: &mut Simple, node: Node, src: &str) -> Option<String> {
    let (raw, cooked) = simple
        .words
        .last()
        .filter(|word| {
            !word.raw.contains('\'')
                && !word.raw.contains('"')
                && !word.cooked.is_empty()
                && word.cooked.bytes().all(|b| b.is_ascii_digit())
        })
        .map(|word| (word.raw.clone(), word.cooked.clone()))
        .filter(|(raw, _)| {
            raw.len() <= node.start_byte()
                && src.get(node.start_byte() - raw.len()..node.start_byte()) == Some(raw.as_str())
        })?;
    simple.words.pop();
    if simple.tokens.last().is_some_and(|token| token == &raw) {
        simple.tokens.pop();
    }
    Some(cooked)
}

/// Cook a command name: every child is one plain word piece. A
/// substitution in the name refuses the stage.
fn cook_name(node: Node, src: &str) -> Option<Word> {
    let mut cooked = String::new();
    for child in node.children(&mut node.walk()) {
        let word = cook_word(child, src)?;
        if word.substituted {
            return None;
        }
        cooked.push_str(&word.cooked);
    }
    text(node, src).map(|raw| Word {
        raw,
        cooked,
        substituted: false,
    })
}

/// Cook one word-ish node into its raw spelling, cooked text, and whether
/// it holds a substitution. A bare `$(` command substitution cooks marked,
/// with its raw spelling kept as its cooked text; any other substitution
/// spelling refuses. A concatenation cooks marked only with exactly one
/// such substitution at its first or last child beside one plain unquoted
/// piece; a quoted piece or a middle substitution refuses. The classifier
/// validates the marked text: cooking alone never reads.
fn cook_word(node: Node, src: &str) -> Option<Word> {
    let raw = text(node, src)?;
    let (cooked, substituted) = match node.kind() {
        "word" | "number" => {
            if !raw.chars().all(is_plain) {
                return None;
            }
            (raw.clone(), false)
        }
        "string" => (cook_quoted(&raw, '"')?, false),
        "raw_string" => (cook_quoted(&raw, '\'')?, false),
        "concatenation" => cook_concatenation(node, src)?,
        "command_substitution" => {
            if !raw.starts_with("$(") {
                return None;
            }
            (raw.clone(), true)
        }
        _ => return None,
    };
    Some(Word {
        raw,
        cooked,
        substituted,
    })
}

/// Cook a concatenation: every piece plain, or exactly one `$(`
/// substitution at an edge beside one plain unquoted piece, marked. Each
/// other shape refuses, as does a substituted shape with a quoted piece or
/// the substitution anywhere but the first or last child.
fn cook_concatenation(node: Node, src: &str) -> Option<(String, bool)> {
    // Each piece's shape: true when it holds the substitution.
    let mut cooked = String::new();
    let mut shape = Vec::new();
    let mut quoted = false;
    for child in node.children(&mut node.walk()) {
        if !child.is_named() {
            let anonymous = text(child, src)?;
            if !anonymous.chars().all(is_plain) {
                return None;
            }
            cooked.push_str(&anonymous);
            shape.push(false);
            continue;
        }
        match child.kind() {
            "command_substitution" => {
                let raw = text(child, src)?;
                if !raw.starts_with("$(") {
                    return None;
                }
                cooked.push_str(&raw);
                shape.push(true);
            }
            "string" => {
                let raw = text(child, src)?;
                cooked.push_str(&cook_quoted(&raw, '"')?);
                quoted = true;
                shape.push(false);
            }
            "raw_string" => {
                let raw = text(child, src)?;
                cooked.push_str(&cook_quoted(&raw, '\'')?);
                quoted = true;
                shape.push(false);
            }
            _ => {
                let word = cook_word(child, src)?;
                if word.substituted {
                    return None;
                }
                cooked.push_str(&word.cooked);
                shape.push(false);
            }
        }
    }
    // One substitution at most, and then only at an edge beside one plain
    // unquoted piece: two substitutions, a middle substitution, or a quoted
    // piece beside it all refuse.
    let marked = shape.iter().filter(|piece| **piece).count();
    if marked > 1 {
        return None;
    }
    if marked == 1 && (quoted || !matches!(shape.as_slice(), [true, false] | [false, true])) {
        return None;
    }
    Some((cooked, marked == 1))
}

/// Cook a quoted spelling: strip the quotes and keep the inner text, after
/// the old double-quote rules (no `$`, no backtick, a backslash only before
/// a plain character) or, for single quotes, with no checks at all.
fn cook_quoted(raw: &str, quote: char) -> Option<String> {
    if raw.len() < 2 || !raw.starts_with(quote) || !raw.ends_with(quote) {
        return None;
    }
    let inner = raw.get(1..raw.len() - 1)?;
    if quote == '\'' {
        return Some(inner.to_string());
    }
    let mut cooked = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '$' | '`' => return None,
            '\\' => {
                let next = chars.next()?;
                if matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                    return None;
                }
                cooked.push('\\');
                cooked.push(next);
            }
            _ => cooked.push(ch),
        }
    }
    Some(cooked)
}

/// The characters an unquoted word may hold: letters, digits and the
/// punctuation the old classifier read plainly. The classifier reuses the
/// set for its substitution checks.
pub(super) fn is_plain(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '_' | '-' | '.' | '/' | ':' | '=' | '@' | '%' | '+' | ','
        )
}

/// The text between two sibling nodes holds only blanks.
fn gap_is_blank(src: &str, end: usize, start: usize) -> bool {
    if start < end {
        return false;
    }
    src.get(end..start)
        .is_some_and(|gap| gap.bytes().all(|b| b == b' ' || b == b'\t'))
}

fn text(node: Node, src: &str) -> Option<String> {
    node.utf8_text(src.as_bytes()).ok().map(str::to_string)
}

impl Simple {
    fn empty(node: Node, src: &str) -> Self {
        let (start, end) = (node.start_byte(), node.end_byte());
        Simple {
            assignments: Vec::new(),
            words: Vec::new(),
            redirects: Vec::new(),
            tokens: Vec::new(),
            text: text_of(node, src),
            start,
            end,
        }
    }

    fn assigned(assignment: (String, String), node: Node, src: &str) -> Self {
        let mut simple = Self::empty(node, src);
        simple.assignments = vec![assignment];
        simple
    }
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
