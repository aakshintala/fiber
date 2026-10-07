//! Syntax highlighting for a reply's code blocks (`docs/tui.md`, "Look"):
//! Fiber's own lexer, one table per language (`docs/dependencies.md`,
//! "Written ourselves"). The tables are static data, so nothing loads when
//! a reply has no code.

use crate::markdown::Role;

/// How a language is read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Tokens: comments, strings, numbers, words and operators.
    Code,
    /// A unified diff, line by line.
    Diff,
    /// Markdown, line by line.
    Markdown,
    /// HTML tags, attributes and text.
    Markup,
}

/// One language's table.
struct Lang {
    mode: Mode,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Characters that open and close a string.
    quotes: &'static [char],
    /// Quotes whose string may run past the end of its line.
    multiline: &'static [char],
    /// Tripled quotes open a string that runs to the same three.
    triple: bool,
    /// `'` opens only a character literal; otherwise it is a lifetime.
    char_literals: bool,
    /// Space-separated word lists.
    keywords: &'static str,
    types: &'static str,
    constants: &'static str,
    /// Characters a word may continue with besides letters, digits and `_`.
    word_extra: &'static [char],
    /// A word after this character is a variable, shown as a constant.
    sigil: Option<char>,
    /// A capitalised word is a type.
    caps_types: bool,
    /// A word followed by this character is a key, shown as a type.
    key_suffix: Option<char>,
    /// Keywords match in any case.
    any_case: bool,
    /// `#` and a word at a line's start are a preprocessor keyword.
    preprocessor: bool,
    /// A word followed by `!` is a macro, shown as a function.
    macros: bool,
}

const BASE: Lang = Lang {
    mode: Mode::Code,
    line_comments: &[],
    block_comment: None,
    quotes: &['"', '\''],
    multiline: &[],
    triple: false,
    char_literals: false,
    keywords: "",
    types: "",
    constants: "true false",
    word_extra: &[],
    sigil: None,
    caps_types: false,
    key_suffix: None,
    any_case: false,
    preprocessor: false,
    macros: false,
};

const C_LIKE: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    caps_types: true,
    ..BASE
};

const RUST: Lang = Lang {
    quotes: &['"'],
    multiline: &['"'],
    char_literals: true,
    macros: true,
    keywords: "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while",
    types: "i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str",
    ..C_LIKE
};

const PYTHON: Lang = Lang {
    line_comments: &["#"],
    triple: true,
    caps_types: true,
    keywords: "and as assert async await break case class continue def del elif else except finally for from global if import in is lambda match nonlocal not or pass raise return try while with yield",
    types: "int float str bool list dict tuple set bytes object",
    constants: "True False None",
    ..BASE
};

const JS_KEYWORDS: &str = "as async await break case catch class const continue debugger default delete do else export extends finally for from function if import in instanceof let new of return static super switch this throw try typeof var void while with yield";

const JS_CONSTANTS: &str = "true false null undefined NaN Infinity";

const JAVASCRIPT: Lang = Lang {
    quotes: &['"', '\'', '`'],
    multiline: &['`'],
    word_extra: &['$'],
    keywords: JS_KEYWORDS,
    constants: JS_CONSTANTS,
    ..C_LIKE
};

const TYPESCRIPT: Lang = Lang {
    keywords: "abstract as async await break case catch class const continue declare default delete do else enum export extends finally for from function if implements import in infer instanceof interface is keyof let namespace new of private protected public readonly return satisfies static super switch this throw try type typeof var void while yield",
    types: "string number boolean any unknown never object bigint symbol",
    ..JAVASCRIPT
};

const GO: Lang = Lang {
    quotes: &['"', '\'', '`'],
    multiline: &['`'],
    keywords: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var",
    types: "bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr any",
    constants: "true false nil iota",
    caps_types: false,
    ..C_LIKE
};

const C_KEYWORDS: &str = "auto break case const continue default do else enum extern for goto if inline register restrict return sizeof static struct switch typedef union volatile while";

const C_TYPES: &str = "char double float int long short signed unsigned void bool size_t ssize_t int8_t int16_t int32_t int64_t uint8_t uint16_t uint32_t uint64_t";

const C: Lang = Lang {
    preprocessor: true,
    caps_types: false,
    keywords: C_KEYWORDS,
    types: C_TYPES,
    ..C_LIKE
};

const CPP: Lang = Lang {
    keywords: "auto break case catch class co_await co_return co_yield concept const const_cast consteval constexpr continue decltype default delete do dynamic_cast else enum explicit export extern final for friend goto if inline mutable namespace new noexcept operator override private protected public reinterpret_cast requires return sizeof static static_assert static_cast struct switch template this throw try typedef typename union using virtual volatile while",
    constants: "nullptr NULL true false",
    ..C
};

const JAVA: Lang = Lang {
    keywords: "abstract assert break case catch class continue default do else enum extends final finally for if implements import instanceof interface native new package permits private protected public record return sealed static super switch synchronized this throw throws transient try var volatile while yield",
    types: "boolean byte char double float int long short void",
    constants: "true false null",
    ..C_LIKE
};

const BASH: Lang = Lang {
    line_comments: &["#"],
    multiline: &['"', '\''],
    sigil: Some('$'),
    keywords: "if then else elif fi for while until do done case esac in function select return break continue local export readonly declare unset shift exit time",
    ..BASE
};

const JSON: Lang = Lang {
    quotes: &['"'],
    constants: "true false null",
    ..BASE
};

const TOML: Lang = Lang {
    line_comments: &["#"],
    triple: true,
    word_extra: &['-'],
    key_suffix: Some('='),
    ..BASE
};

const YAML: Lang = Lang {
    line_comments: &["#"],
    word_extra: &['-'],
    key_suffix: Some(':'),
    constants: "true false null yes no on off",
    ..BASE
};

const CSS: Lang = Lang {
    block_comment: Some(("/*", "*/")),
    word_extra: &['-'],
    key_suffix: Some(':'),
    constants: "important inherit initial none auto",
    ..BASE
};

const SQL: Lang = Lang {
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    any_case: true,
    keywords: "add all alter and as asc begin between by case commit create default delete desc distinct drop else end exists foreign from full group having in index inner insert into is join key left like limit not offset on or order outer primary references returning right rollback select set table then union update values when where with",
    types: "bigint boolean char date decimal double float int integer numeric real serial smallint text timestamp varchar",
    constants: "true false null",
    ..BASE
};

const DIFF: Lang = Lang {
    mode: Mode::Diff,
    ..BASE
};

const MARKDOWN: Lang = Lang {
    mode: Mode::Markdown,
    ..BASE
};

const HTML: Lang = Lang {
    mode: Mode::Markup,
    ..BASE
};

/// The language a fence's info string names: its first word, in any case,
/// or a common alias. `None` for any other tag.
fn language(info: &str) -> Option<&'static Lang> {
    let word = info
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match word.as_str() {
        "rust" | "rs" => &RUST,
        "python" | "py" | "python3" => &PYTHON,
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => &JAVASCRIPT,
        "typescript" | "ts" | "tsx" | "mts" | "cts" => &TYPESCRIPT,
        "go" | "golang" => &GO,
        "c" | "h" => &C,
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => &CPP,
        "java" => &JAVA,
        "bash" | "sh" | "shell" | "zsh" => &BASH,
        "json" | "jsonc" => &JSON,
        "toml" => &TOML,
        "yaml" | "yml" => &YAML,
        "html" | "htm" | "xml" => &HTML,
        "css" => &CSS,
        "sql" => &SQL,
        "diff" | "patch" => &DIFF,
        "markdown" | "md" => &MARKDOWN,
        _ => return None,
    })
}

/// The code's lines as runs of one role each, or `None` when `lang` names
/// no language this lexer reads. One inner list per line of `code` split on
/// `\n`; a line's strings joined are that line exactly.
pub(crate) fn spans(lang: &str, code: &str) -> Option<Vec<Vec<(Role, String)>>> {
    let lang = language(lang)?;
    let mut lexer = Lexer {
        code,
        at: 0,
        out: Vec::new(),
    };
    match lang.mode {
        Mode::Code => lexer.code(lang),
        Mode::Diff => lexer.lines(diff_line),
        Mode::Markdown => lexer.lines(markdown_line),
        Mode::Markup => lexer.markup(),
    }
    Some(split_lines(&lexer.out))
}

/// Splits role runs into lines, merging neighbours of the same role.
fn split_lines(runs: &[(Role, &str)]) -> Vec<Vec<(Role, String)>> {
    let mut lines: Vec<Vec<(Role, String)>> = vec![Vec::new()];
    for (role, text) in runs {
        for (at, part) in text.split('\n').enumerate() {
            if at > 0 {
                lines.push(Vec::new());
            }
            let Some(line) = lines.last_mut() else {
                continue;
            };
            if part.is_empty() {
                continue;
            }
            match line.last_mut() {
                Some((last, joined)) if last == role => joined.push_str(part),
                _ => line.push((*role, part.to_owned())),
            }
        }
    }
    lines
}

/// A diff line's role.
fn diff_line(line: &str) -> Vec<(Role, usize)> {
    let role = if ["+++", "---", "diff ", "index "]
        .iter()
        .any(|head| line.starts_with(head))
    {
        Role::Keyword
    } else if line.starts_with("@@") {
        Role::Type
    } else if line.starts_with('+') {
        Role::String
    } else if line.starts_with('-') {
        Role::Constant
    } else {
        Role::CodeText
    };
    vec![(role, line.len())]
}

/// A markdown line's runs: a heading or fence line whole, a list marker,
/// and inline code.
fn markdown_line(line: &str) -> Vec<(Role, usize)> {
    let body = line.trim_start();
    let indent = line.len().saturating_sub(body.len());
    if body.starts_with('#') {
        return vec![(Role::Keyword, line.len())];
    }
    if body.starts_with("```") || body.starts_with("~~~") {
        return vec![(Role::Comment, line.len())];
    }
    let mut runs = vec![(Role::CodeText, indent)];
    let digits = body.len().saturating_sub(
        body.trim_start_matches(|ch: char| ch.is_ascii_digit())
            .len(),
    );
    let after_digits = body.get(digits..).unwrap_or_default();
    let marker = if ["- ", "* ", "+ "]
        .iter()
        .any(|bullet| body.starts_with(bullet))
    {
        1
    } else if digits > 0 && (after_digits.starts_with(". ") || after_digits.starts_with(") ")) {
        digits.saturating_add(1)
    } else {
        0
    };
    runs.push((Role::Operator, marker));
    let mut rest = body.get(marker..).unwrap_or_default();
    while !rest.is_empty() {
        let (role, len) = match rest.strip_prefix('`') {
            Some(after) => (
                Role::String,
                after
                    .find('`')
                    .map_or(rest.len(), |end| end.saturating_add(2)),
            ),
            None => (Role::CodeText, rest.find('`').unwrap_or(rest.len())),
        };
        debug_assert!(len > 0, "each run takes at least one byte");
        runs.push((role, len));
        rest = rest.get(len..).unwrap_or_default();
    }
    runs
}

/// The lexer's state: the code, the byte it has reached and the runs so far.
struct Lexer<'a> {
    code: &'a str,
    at: usize,
    out: Vec<(Role, &'a str)>,
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        self.code.get(self.at..).unwrap_or_default()
    }

    /// Takes the next `len` bytes as one run of `role`.
    fn emit(&mut self, role: Role, len: usize) {
        let end = self.at.saturating_add(len).min(self.code.len());
        if let Some(text) = self.code.get(self.at..end)
            && !text.is_empty()
        {
            self.out.push((role, text));
        }
        self.at = end;
    }

    /// Bytes from the start of the rest while `keep` holds for each char.
    fn span_while(&self, keep: impl Fn(char) -> bool) -> usize {
        self.rest()
            .char_indices()
            .find(|(_, ch)| !keep(*ch))
            .map_or(self.rest().len(), |(at, _)| at)
    }

    /// Reads each line with `read`, which returns its runs as lengths.
    fn lines(&mut self, read: fn(&str) -> Vec<(Role, usize)>) {
        while self.at < self.code.len() {
            let before = self.at;
            let line = self.rest().split('\n').next().unwrap_or_default();
            for (role, len) in read(line) {
                self.emit(role, len);
            }
            self.emit(Role::CodeText, 1);
            debug_assert!(self.at > before, "each line takes at least one byte");
        }
    }

    /// Reads tokens of a code language.
    fn code(&mut self, lang: &Lang) {
        let mut line_start = true;
        while let Some(ch) = self.rest().chars().next() {
            let before = self.at;
            let rest = self.rest();
            let starts_line = line_start;
            line_start = ch == '\n' || (line_start && ch.is_whitespace());
            if let Some((open, close)) = lang.block_comment
                && rest.starts_with(open)
            {
                let len = rest
                    .get(open.len()..)
                    .and_then(|body| body.find(close))
                    .map_or(rest.len(), |end| {
                        end.saturating_add(open.len()).saturating_add(close.len())
                    });
                self.emit(Role::Comment, len);
            } else if lang.line_comments.iter().any(|open| rest.starts_with(open)) {
                self.emit(Role::Comment, rest.find('\n').unwrap_or(rest.len()));
            } else if lang.preprocessor && starts_line && ch == '#' {
                let len = 1 + rest
                    .get(1..)
                    .unwrap_or_default()
                    .find(|ch: char| !ch.is_alphanumeric() && ch != '_')
                    .unwrap_or(rest.len().saturating_sub(1));
                self.emit(Role::Keyword, len);
            } else if lang.quotes.contains(&ch) {
                let len = string_len(lang, rest, ch);
                self.emit(Role::String, len);
            } else if lang.char_literals && ch == '\'' {
                self.char_literal(rest);
            } else if ch.is_ascii_digit() {
                let len = number_len(rest);
                self.emit(Role::Number, len);
            } else if lang.sigil == Some(ch) {
                let len = 1 + rest
                    .get(1..)
                    .unwrap_or_default()
                    .find(|ch: char| !is_word(lang, ch))
                    .unwrap_or(rest.len().saturating_sub(1));
                self.emit(Role::Constant, len);
            } else if ch.is_alphabetic()
                || ch == '_'
                || (lang.word_extra.contains(&ch) && ch != '-')
            {
                self.word(lang);
            } else if OPERATORS.contains(ch) {
                let len = self.span_while(|ch| OPERATORS.contains(ch));
                self.emit(Role::Operator, len);
            } else {
                self.emit(Role::CodeText, ch.len_utf8());
            }
            debug_assert!(self.at > before, "each token takes at least one byte");
        }
    }

    /// A word: a keyword, type, constant, function, key or plain name.
    fn word(&mut self, lang: &Lang) {
        let len = self.span_while(|ch| is_word(lang, ch));
        let rest = self.rest();
        let word = rest.get(..len).unwrap_or_default();
        let after = rest.get(len..).unwrap_or_default();
        let next = after.trim_start_matches([' ', '\t']).chars().next();
        let lower;
        let key = if lang.any_case {
            lower = word.to_ascii_lowercase();
            lower.as_str()
        } else {
            word
        };
        let listed = |list: &str| list.split(' ').any(|listed| listed == key);
        let role = if listed(lang.keywords) {
            Role::Keyword
        } else if listed(lang.constants) {
            Role::Constant
        } else if listed(lang.types) || (lang.key_suffix.is_some() && next == lang.key_suffix) {
            Role::Type
        } else if after.starts_with('(')
            || (lang.macros && after.starts_with('!') && !after.starts_with("!="))
        {
            Role::Function
        } else if word.chars().count() > 1
            && word
                .chars()
                .all(|ch| ch.is_uppercase() || ch.is_ascii_digit() || ch == '_')
            && word.chars().any(char::is_alphabetic)
        {
            Role::Constant
        } else if lang.caps_types && word.chars().next().is_some_and(char::is_uppercase) {
            Role::Type
        } else {
            Role::CodeText
        };
        self.emit(role, len);
    }

    /// `'`: a character literal when one closes it, as `'a'` or `'\n'`;
    /// otherwise a lifetime or label, shown plain.
    fn char_literal(&mut self, rest: &str) {
        let body = rest.get(1..).unwrap_or_default();
        let len = if let Some(escaped) = body.strip_prefix('\\') {
            let skip = escaped.chars().next().map_or(0, char::len_utf8);
            escaped.get(skip..).and_then(|tail| {
                tail.find(['\'', '\n'])
                    .filter(|end| tail.get(*end..).is_some_and(|end| end.starts_with('\'')))
                    .map(|end| end.saturating_add(skip).saturating_add(3))
            })
        } else {
            body.chars()
                .next()
                .filter(|ch| *ch != '\n')
                .map(char::len_utf8)
                .filter(|width| {
                    body.get(*width..)
                        .is_some_and(|tail| tail.starts_with('\''))
                })
                .map(|width| width.saturating_add(2))
        };
        match len {
            Some(len) => self.emit(Role::String, len),
            None => self.emit(Role::CodeText, 1),
        }
    }

    /// Reads HTML: comments, tags with their attributes, entities and text.
    fn markup(&mut self) {
        let mut in_tag = false;
        while let Some(ch) = self.rest().chars().next() {
            let before = self.at;
            let rest = self.rest();
            if rest.starts_with("<!--") {
                let len = rest
                    .find("-->")
                    .map_or(rest.len(), |end| end.saturating_add(3));
                self.emit(Role::Comment, len);
            } else if ch == '<' {
                let slash = usize::from(rest.get(1..).is_some_and(|tail| tail.starts_with('/')));
                self.emit(Role::Operator, 1 + slash);
                let len = self.span_while(|ch| ch.is_alphanumeric() || ch == '-' || ch == '!');
                self.emit(Role::Keyword, len);
                in_tag = true;
            } else if in_tag && (ch == '>' || rest.starts_with("/>")) {
                self.emit(Role::Operator, if ch == '>' { 1 } else { 2 });
                in_tag = false;
            } else if in_tag && (ch == '"' || ch == '\'') {
                let len = rest
                    .get(1..)
                    .and_then(|tail| tail.find(ch))
                    .map_or(rest.len(), |end| end.saturating_add(2));
                self.emit(Role::String, len);
            } else if in_tag && (ch.is_alphabetic() || ch == '_') {
                let len =
                    self.span_while(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | ':'));
                self.emit(Role::Type, len);
            } else if in_tag && ch == '=' {
                self.emit(Role::Operator, 1);
            } else if !in_tag && ch == '&' {
                let len = rest
                    .find(|ch: char| ch == ';' || ch.is_whitespace() || ch == '<')
                    .filter(|end| rest.get(*end..).is_some_and(|tail| tail.starts_with(';')))
                    .map_or(1, |end| end.saturating_add(1));
                self.emit(Role::Constant, len);
            } else if in_tag {
                self.emit(Role::CodeText, ch.len_utf8());
            } else {
                let len = rest.find(['<', '&']).unwrap_or(rest.len()).max(1);
                self.emit(Role::CodeText, len);
            }
            debug_assert!(self.at > before, "each token takes at least one byte");
        }
    }
}

/// Characters that make up operators.
const OPERATORS: &str = "=+-*/%<>!&|^~?:";

/// Whether `ch` continues a word in `lang`.
fn is_word(lang: &Lang, ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || lang.word_extra.contains(&ch)
}

/// The length of the string opening `rest` with `quote`, to its closing
/// quote. A backslash escapes the next character. A string that may not
/// span lines ends before a newline that comes first; an unclosed one runs
/// to the end of the code.
fn string_len(lang: &Lang, rest: &str, quote: char) -> usize {
    let tripled: String = [quote; 3].iter().collect();
    if lang.triple && rest.starts_with(&tripled) {
        return rest
            .get(3..)
            .and_then(|body| body.find(&tripled))
            .map_or(rest.len(), |end| end.saturating_add(6));
    }
    let multiline = lang.multiline.contains(&quote);
    let mut escaped = false;
    for (at, ch) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return at.saturating_add(ch.len_utf8());
        } else if ch == '\n' && !multiline {
            return at;
        }
    }
    rest.len()
}

/// The length of the number opening `rest`: digits, letters for a radix,
/// exponent or suffix, `_`, and a `.` followed by a digit.
fn number_len(rest: &str) -> usize {
    let mut chars = rest.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        let decimal = ch == '.' && chars.peek().is_some_and(|(_, next)| next.is_ascii_digit());
        if !(ch.is_ascii_alphanumeric() || ch == '_' || decimal) {
            return at;
        }
    }
    rest.len()
}

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;
