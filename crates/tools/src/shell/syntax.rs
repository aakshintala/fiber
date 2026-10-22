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
//! concatenation. Allowed tokens: `;`, `&&`, `||`, `|`, `|&` (refused, but
//! recognised), `=`, `>`, `>&`, `<`. A `|&` pipe, `&`, a newline between
//! statements, or any other token or kind, refuses the stage or the call.

use tree_sitter::Node;

/// The longest command the recogniser parses; longer calls classify with no
/// subject. The bound keeps one classification cheap: the tree walk below is
/// iterative, so even a 10,000-byte `ls&&ls&&...` list cannot overflow the
/// stack it runs on.
pub(super) const MAX_COMMAND_BYTES: usize = 10_000;

/// One parsed call: its `&&`/`||`/`;` elements in source order. There is no
/// join information; part 1 treats every element as one more stage.
pub(super) struct Script {
    pub(super) elements: Vec<Element>,
}

/// One `&&`/`||`/`;` element: its `|` pipeline stages in source order.
pub(super) struct Element {
    pub(super) stages: Vec<Stage>,
}

/// One pipeline stage: a simple command the recogniser read fully, or an
/// opaque statement (a subshell, a loop, a refused redirect) that still
/// classifies but never reads.
pub(super) enum Stage {
    Simple(Simple),
    Opaque,
}

/// One simple command: its leading `NAME=value` assignments, its operand
/// words after quote removal, its redirects in source order, and the tokens
/// the subject is built from: the operand words and redirect spellings in
/// source order, assignments excluded.
pub(super) struct Simple {
    pub(super) assignments: Vec<(String, String)>,
    pub(super) words: Vec<Word>,
    pub(super) redirects: Vec<Redirect>,
    pub(super) tokens: Vec<String>,
}

/// One operand word: its raw spelling and its cooked text after quote
/// removal. `bare` is false for a quoted spelling, which never counts as a
/// redirect descriptor no matter its text.
pub(super) struct Word {
    pub(super) raw: String,
    pub(super) cooked: String,
    pub(super) bare: bool,
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
    loop {
        let statement = next?;
        if statement.kind() == "comment" {
            return None;
        }
        push_statement(statement, command, &mut elements);
        let Some(separator) = children.next() else {
            break;
        };
        if separator.kind() != ";" {
            return None;
        }
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
fn push_statement(node: Node, src: &str, elements: &mut Vec<Element>) {
    match node.kind() {
        "command" => elements.push(Element {
            stages: vec![cook_command(node, src).map_or(Stage::Opaque, Stage::Simple)],
        }),
        "list" => push_list(node, src, elements),
        "pipeline" => elements.push(pipeline(node, src)),
        "redirected_statement" => elements.extend(redirected(node, src)),
        "variable_assignment" => elements.push(Element {
            stages: vec![
                cook_assignment(node, src).map_or(Stage::Opaque, |assignments| {
                    Stage::Simple(Simple::assigned(assignments))
                }),
            ],
        }),
        _ => elements.push(Element {
            stages: vec![Stage::Opaque],
        }),
    }
}

/// Flatten a left-nested `&&`/`||` list into elements in source order, with
/// an explicit stack instead of recursion.
fn push_list(node: Node, src: &str, elements: &mut Vec<Element>) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() != "list" {
            push_statement(node, src, elements);
            continue;
        }
        let mut cursor = node.walk();
        let mut children = node.children(&mut cursor);
        let (Some(left), Some(operator), Some(right)) =
            (children.next(), children.next(), children.next())
        else {
            elements.push(Element {
                stages: vec![Stage::Opaque],
            });
            continue;
        };
        if children.next().is_some() || (operator.kind() != "&&" && operator.kind() != "||") {
            elements.push(Element {
                stages: vec![Stage::Opaque],
            });
            continue;
        }
        stack.push(right);
        stack.push(left);
    }
}

/// Convert one `|` pipeline into a single element. A `|&` pipe makes the
/// whole element opaque; any other surprise does the same.
fn pipeline(node: Node, src: &str) -> Element {
    let opaque = || Element {
        stages: vec![Stage::Opaque],
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
                stages.push(cook_command(stage, src).map_or(Stage::Opaque, Stage::Simple));
            }
            "redirected_statement" => {
                let mut nested = redirected(stage, src);
                let single =
                    nested.len() == 1 && nested.first().is_some_and(|e| e.stages.len() == 1);
                if !single {
                    return opaque();
                }
                let Some(mut only) = nested.pop() else {
                    return opaque();
                };
                let Some(first) = only.stages.pop() else {
                    return opaque();
                };
                stages.push(first);
            }
            "variable_assignment" => stages.push(
                cook_assignment(stage, src).map_or(Stage::Opaque, |assignments| {
                    Stage::Simple(Simple::assigned(assignments))
                }),
            ),
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
    Element { stages }
}

/// Convert a redirected statement: its body plus each redirect, the leading
/// ones onto the first stage and the trailing ones onto the last. A refused
/// redirect opaques its stage; anything but one body opaques the call.
fn redirected(node: Node, src: &str) -> Vec<Element> {
    let opaque = || {
        vec![Element {
            stages: vec![Stage::Opaque],
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
                if body.is_none_or(|b: Node| child.start_byte() < b.start_byte()) {
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
    push_statement(body, src, &mut elements);
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
    let attached = match stage {
        Stage::Simple(simple) => push_redirect(simple, redirect, src, !trailing),
        Stage::Opaque => true,
    };
    if !attached {
        *stage = Stage::Opaque;
    }
}

/// Build one simple command from a `command` node: leading assignments, one
/// name, plain words, and redirects in source order. Gaps between children
/// hold only blanks; anything else (a carriage return splitting two words)
/// refuses the stage, as does anything the recogniser does not implement.
fn cook_command(node: Node, src: &str) -> Option<Simple> {
    let mut simple = Simple::empty();
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
            "word" | "number" | "string" | "raw_string" | "concatenation" => {
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
                value = Some(word.cooked);
            }
            "=" => {}
            _ => return None,
        }
    }
    let name = name.filter(|name| {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })?;
    Some((name, value.unwrap_or_default()))
}

/// Validate one redirect and push it onto the stage: a bare all-digit word
/// right before a descriptor-less redirect is its descriptor, the way the
/// shell reads `0<f` but keeps the quoted `"2"` in `"2">/dev/null` an
/// operand. Anything outside the allow-list refuses the stage.
fn push_redirect(simple: &mut Simple, node: Node, src: &str, leading: bool) -> bool {
    let mut descriptor = None;
    let mut operator = None;
    let mut destination = None;
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
                if destination.is_some() {
                    return false;
                }
                let Some(word) = cook_word(child, src) else {
                    return false;
                };
                destination = Some(word);
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
    if descriptor.is_none() {
        descriptor = steal_descriptor(simple, node, src);
    }
    let redirect = match operator {
        ">" => {
            if !matches!(descriptor.as_deref(), None | Some("1") | Some("2")) {
                return false;
            }
            let Some(destination) = destination else {
                return false;
            };
            if destination.cooked != "/dev/null" {
                return false;
            }
            Redirect::Discard
        }
        ">&" => {
            if !matches!(descriptor.as_deref(), None | Some("1") | Some("2")) {
                return false;
            }
            let Some(destination) = destination else {
                return false;
            };
            if destination.cooked != "1" && destination.cooked != "2" {
                return false;
            }
            Redirect::Merge
        }
        "<" => {
            if !matches!(descriptor.as_deref(), None | Some("0")) {
                return false;
            }
            let Some(destination) = destination else {
                return false;
            };
            if destination.cooked.starts_with("/dev/tcp/")
                || destination.cooked.starts_with("/dev/udp/")
            {
                return false;
            }
            Redirect::Input(destination)
        }
        _ => return false,
    };
    let Some(raw) = text(node, src) else {
        return false;
    };
    simple.redirects.push(redirect);
    if leading {
        simple.tokens.insert(0, raw);
    } else {
        simple.tokens.push(raw);
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
            word.bare && !word.cooked.is_empty() && word.cooked.bytes().all(|b| b.is_ascii_digit())
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

/// Cook a command name: every child is one plain word piece.
fn cook_name(node: Node, src: &str) -> Option<Word> {
    let mut cooked = String::new();
    let mut bare = true;
    for child in node.children(&mut node.walk()) {
        let piece = cook_word(child, src)?;
        bare = bare && piece.bare;
        cooked.push_str(&piece.cooked);
    }
    text(node, src).map(|raw| Word { raw, cooked, bare })
}

/// Cook one word-ish node into its raw spelling and cooked text.
fn cook_word(node: Node, src: &str) -> Option<Word> {
    let raw = text(node, src)?;
    let (cooked, bare) = match node.kind() {
        "word" | "number" => {
            if !raw.chars().all(is_plain) {
                return None;
            }
            (raw.clone(), true)
        }
        "string" => (cook_quoted(&raw, '"')?, false),
        "raw_string" => (cook_quoted(&raw, '\'')?, false),
        "concatenation" => {
            let mut cooked = String::new();
            let mut bare = true;
            for child in node.children(&mut node.walk()) {
                if !child.is_named() {
                    let anonymous = text(child, src)?;
                    if !anonymous.chars().all(is_plain) {
                        return None;
                    }
                    cooked.push_str(&anonymous);
                    continue;
                }
                let piece = cook_word(child, src)?;
                bare = bare && piece.bare;
                cooked.push_str(&piece.cooked);
            }
            (cooked, bare)
        }
        _ => return None,
    };
    Some(Word { raw, cooked, bare })
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
/// punctuation the old classifier read plainly.
fn is_plain(ch: char) -> bool {
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
    fn empty() -> Self {
        Simple {
            assignments: Vec::new(),
            words: Vec::new(),
            redirects: Vec::new(),
            tokens: Vec::new(),
        }
    }

    fn assigned(assignments: (String, String)) -> Self {
        Simple {
            assignments: vec![assignments],
            words: Vec::new(),
            redirects: Vec::new(),
            tokens: Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
