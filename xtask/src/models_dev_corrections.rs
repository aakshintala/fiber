//! The cited corrections to the generated model lists, and the checks
//! that keep each entry honest (`docs/model-routing.md`, "What a provider
//! extension declares").

use serde_json::{Map, Value};

use crate::models_dev_table::Package;

/// The cited corrections file, from the repository root: each entry
/// names a package, a model and the derived fields it overrides, with
/// the vendor page that says so (`source`) and the models.dev pull
/// request fixing it upstream (`upstream`). The generator applies the
/// entries after the per-package table and deletes an entry once
/// models.dev agrees with it (`docs/model-routing.md`, "What a provider
/// extension declares").
pub(crate) const CORRECTIONS: &str = "providers/models-corrections.toml";

/// One entry of the corrections file.
#[derive(Clone, Debug)]
pub(crate) struct Correction {
    /// The package file's stem, such as `openai`, or its full path.
    pub(crate) package: String,
    /// The model id the entry overrides.
    pub(crate) model: String,
    /// The derived fields the entry overrides.
    pub(crate) fields: Map<String, Value>,
    /// The vendor page that says so.
    pub(crate) source: String,
    /// The models.dev pull request fixing it upstream.
    pub(crate) upstream: String,
}

/// The derived model-level fields a correction may override: every
/// other model key comes from the per-package table alone.
const OVERRIDABLE_FIELDS: [&str; 6] = [
    "name",
    "protocol",
    "context_window",
    "max_output_tokens",
    "input",
    "cost",
];

/// The model keys a correction must never name: the table owns them,
/// with `id` as the entry's own key.
const TABLE_ONLY_FIELDS: [&str; 6] = [
    "id",
    "thinking_levels",
    "thinking_default",
    "compat",
    "base_url",
    "web_search",
];

/// One corrections entry before its required keys are checked.
#[derive(Default)]
struct RawEntry {
    /// The `package` value, if the entry names one.
    package: Option<Value>,
    /// The `model` value, if the entry names one.
    model: Option<Value>,
    /// The `fields` table, if the entry names one.
    fields: Option<Map<String, Value>>,
    /// Whether `fields` held a non-table: kept apart so the error names it.
    fields_bad: bool,
    /// The `source` value, if the entry names one.
    source: Option<Value>,
    /// The `upstream` value, if the entry names one.
    upstream: Option<Value>,
}

// debt: the hand parser reads only the TOML subset the corrections schema needs (strings, integers, floats, booleans, arrays and inline tables); upgrade to the `toml` crate when the schema needs datetimes, multiline strings or nesting the hand parser cannot read.
/// A cursor over corrections text: every read is a checked `get`, so a
/// rough edge is an error, never a panic.
struct Parser<'a> {
    /// The file path, naming every error.
    source: &'a str,
    /// The whole text, for substring reads.
    text: &'a str,
    /// Its bytes, for checked single-byte reads.
    bytes: &'a [u8],
    /// The next byte to read: always a string boundary.
    pos: usize,
    /// The 1-based entry being read: 0 before the first `[[correction]]`.
    entry: usize,
    /// Whether a `[correction.fields]` header opened the current section.
    fields: bool,
}

impl Parser<'_> {
    /// Names `detail` as this file's error.
    fn fail<T>(&self, detail: String) -> Result<T, String> {
        Err(format!("{}: {detail}", self.source))
    }

    /// The next byte, if any.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// The next character, if any.
    fn peek_char(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    /// Moves past the next character.
    fn bump(&mut self) {
        if let Some(next) = self.peek_char() {
            self.pos += next.len_utf8();
        }
    }

    /// The text from `start` to the cursor.
    fn since(&self, start: usize) -> Option<&str> {
        self.text.get(start..self.pos)
    }

    /// Moves past one `byte`, or errs naming what was wanted.
    fn eat(&mut self, byte: u8, want: &str) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.bump();
            Ok(())
        } else {
            self.fail(format!("reads no {want}"))
        }
    }

    /// Moves past blank space, newlines and `#` comments.
    fn skip_gap(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => self.bump(),
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// Whether the cursor holds `word` exactly.
    fn starts(&self, word: &str) -> bool {
        let end = self.pos + word.len();
        matches!(self.text.get(self.pos..end), Some(head) if head == word)
    }

    /// Whether the cursor holds `word` with a non-key byte after it.
    fn holds(&self, word: &str) -> bool {
        let end = self.pos + word.len();
        match self.text.get(self.pos..end) {
            Some(head) if head == word => match self.text.get(end..) {
                None => true,
                Some(tail) => !tail
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-'),
            },
            _ => false,
        }
    }

    /// Moves past `word`, which `holds` already confirmed.
    fn take(&mut self, word: &str) {
        self.pos += word.len();
    }

    /// One bare key: ASCII letters, digits, `_` and `-`.
    fn bare_key(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self
            .peek_char()
            .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-')
        {
            self.bump();
        }
        match self.since(start) {
            Some(key) if !key.is_empty() => Ok(key.to_owned()),
            _ => self.fail("reads no key".to_owned()),
        }
    }

    /// One TOML value: a string, integer, float, boolean, array or
    /// inline table. Multiline strings and dates have no reading here.
    fn value(&mut self) -> Result<Value, String> {
        self.skip_gap();
        match self.peek() {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            Some(b't') if self.holds("true") => {
                self.take("true");
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.holds("false") => {
                self.take("false");
                Ok(Value::Bool(false))
            }
            Some(b'[') => self.array(),
            Some(b'{') => self.inline_table(),
            Some(b'-') | Some(b'+') | Some(b'0'..=b'9') => self.number(),
            Some(_) => self.fail("reads an unexpected byte".to_owned()),
            None => self.fail("ends inside a value".to_owned()),
        }
    }

    /// One `"`-quoted string with its closing quote consumed.
    fn basic_string(&mut self) -> Result<Value, String> {
        self.bump();
        if self.peek() == Some(b'"') {
            self.bump();
            if self.peek() == Some(b'"') {
                return self.fail("reads a multiline string, which has no reading".to_owned());
            }
            return Ok(Value::from(""));
        }
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.fail("ends inside a string".to_owned()),
                Some('"') => {
                    self.bump();
                    return Ok(Value::from(text));
                }
                Some('\\') => {
                    self.bump();
                    text.push(self.escape()?);
                }
                Some(next) => {
                    self.bump();
                    text.push(next);
                }
            }
        }
    }

    /// The escape after a `\`: its character.
    fn escape(&mut self) -> Result<char, String> {
        match self.peek_char() {
            Some('"') => {
                self.bump();
                Ok('"')
            }
            Some('\\') => {
                self.bump();
                Ok('\\')
            }
            Some('b') => {
                self.bump();
                Ok('\u{08}')
            }
            Some('f') => {
                self.bump();
                Ok('\u{0C}')
            }
            Some('n') => {
                self.bump();
                Ok('\n')
            }
            Some('r') => {
                self.bump();
                Ok('\r')
            }
            Some('t') => {
                self.bump();
                Ok('\t')
            }
            Some('u') => {
                self.bump();
                self.hex(4)
            }
            Some('U') => {
                self.bump();
                self.hex(8)
            }
            _ => self.fail("reads an unknown escape".to_owned()),
        }
    }

    /// `digits` hexadecimal digits as one character.
    fn hex(&mut self, digits: usize) -> Result<char, String> {
        let mut code: u32 = 0;
        for _ in 0..digits {
            match self.peek_char() {
                Some(next) if next.is_ascii_hexdigit() => {
                    self.bump();
                    code = code * 16 + next.to_digit(16).unwrap_or(0);
                }
                _ => return self.fail("reads a short unicode escape".to_owned()),
            }
        }
        char::from_u32(code).ok_or_else(|| format!("{}: reads no character", self.source))
    }

    /// One `'`-quoted string with no escapes.
    fn literal_string(&mut self) -> Result<Value, String> {
        self.bump();
        if self.peek() == Some(b'\'') {
            self.bump();
            if self.peek() == Some(b'\'') {
                return self.fail("reads a multiline string, which has no reading".to_owned());
            }
            return Ok(Value::from(""));
        }
        let mut text = String::new();
        loop {
            match self.peek_char() {
                None => return self.fail("ends inside a string".to_owned()),
                Some('\'') => {
                    self.bump();
                    return Ok(Value::from(text));
                }
                Some(next) => {
                    self.bump();
                    text.push(next);
                }
            }
        }
    }

    /// One integer or float.
    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'-') | Some(b'+')) {
            self.bump();
        }
        while self.peek_char().is_some_and(|next| {
            next.is_ascii_alphanumeric() || next == '_' || next == '.' || next == '+' || next == '-'
        }) {
            self.bump();
        }
        let token = self.since(start).unwrap_or("");
        let digits: String = token.chars().filter(|next| *next != '_').collect();
        if let Ok(int) = digits.parse::<i64>() {
            return Ok(Value::from(int));
        }
        if let Ok(float) = digits.parse::<f64>() {
            return Ok(Value::from(float));
        }
        self.fail(format!("reads a malformed number `{token}`"))
    }

    /// One `[...]` array, spanning lines.
    fn array(&mut self) -> Result<Value, String> {
        self.bump();
        self.skip_gap();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.bump();
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_gap();
            match self.peek() {
                Some(b',') => {
                    self.bump();
                    self.skip_gap();
                    if self.peek() == Some(b']') {
                        self.bump();
                        return Ok(Value::Array(items));
                    }
                }
                Some(b']') => {
                    self.bump();
                    return Ok(Value::Array(items));
                }
                _ => return self.fail("reads no `,` or `]`".to_owned()),
            }
        }
    }

    /// One `{...}` inline table.
    fn inline_table(&mut self) -> Result<Value, String> {
        self.bump();
        self.skip_gap();
        let mut table = Map::new();
        if self.peek() == Some(b'}') {
            self.bump();
            return Ok(Value::Object(table));
        }
        loop {
            let key = self.bare_key()?;
            self.skip_gap();
            self.eat(b'=', "`=`")?;
            let value = self.value()?;
            if table.insert(key.clone(), value).is_some() {
                return self.fail(format!("names `{key}` twice"));
            }
            self.skip_gap();
            match self.peek() {
                Some(b',') => {
                    self.bump();
                    self.skip_gap();
                    if self.peek() == Some(b'}') {
                        self.bump();
                        return Ok(Value::Object(table));
                    }
                }
                Some(b'}') => {
                    self.bump();
                    return Ok(Value::Object(table));
                }
                _ => return self.fail("reads no `,` or `}`".to_owned()),
            }
        }
    }

    /// One `[[correction]]` header: the caller pushes a fresh entry,
    /// which becomes current.
    fn array_header(&mut self) -> Result<(), String> {
        self.take("[[");
        self.skip_gap();
        let name = self.bare_key()?;
        self.skip_gap();
        self.eat(b']', "`]]`")?;
        self.eat(b']', "`]]`")?;
        self.end_header()?;
        if name != "correction" {
            return self.fail(format!("names unknown entry `{name}`"));
        }
        Ok(())
    }

    /// One `[correction.fields]` header: the current entry takes fields.
    fn table_header(&mut self, entries: &mut [RawEntry]) -> Result<(), String> {
        self.bump();
        self.skip_gap();
        let mut parts = vec![self.bare_key()?];
        while self.peek() == Some(b'.') {
            self.bump();
            self.skip_gap();
            parts.push(self.bare_key()?);
            self.skip_gap();
        }
        self.eat(b']', "`]`")?;
        self.end_header()?;
        if parts == ["correction", "fields"] {
            if self.entry == 0 {
                return self.fail("names a `fields` table outside any `[[correction]]`".to_owned());
            }
            if let Some(current) = entries.get_mut(self.entry - 1) {
                if current.fields.is_some() || current.fields_bad {
                    return self.fail(format!("correction {} names `fields` twice", self.entry));
                }
                current.fields = Some(Map::new());
            }
            self.fields = true;
            return Ok(());
        }
        self.fail(format!("names unknown table `{}`", parts.join(".")))
    }

    /// Moves past blank space and `#` comments, stopping at a newline.
    fn skip_line_gap(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') => self.bump(),
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n') | Some(b'\r')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// The end of a top-level line: only blank space or a comment
    /// follows the item; returns whether the file ends here.
    fn end_line(&mut self, what: &str) -> Result<bool, String> {
        self.skip_line_gap();
        match self.peek() {
            None => Ok(true),
            Some(b'\n') | Some(b'\r') => {
                self.bump();
                Ok(false)
            }
            _ => self.fail(format!("reads trailing text after {what}")),
        }
    }

    /// The end of a header line: only blank space or a comment follows.
    /// The newline stays for the file loop to consume.
    fn end_header(&mut self) -> Result<(), String> {
        self.skip_line_gap();
        match self.peek() {
            None | Some(b'\n') | Some(b'\r') => Ok(()),
            _ => self.fail("reads trailing text after the header".to_owned()),
        }
    }

    /// One `key = value` line: in the current entry, or in its fields
    /// after a `[correction.fields]` header. Before the first
    /// `[[correction]]` any key is an error.
    fn key_value(&mut self, entries: &mut [RawEntry]) -> Result<(), String> {
        let key = self.bare_key()?;
        self.skip_gap();
        self.eat(b'=', "`=`")?;
        let number = self.entry;
        let in_fields = self.fields;
        let value = self.value().map_err(|detail| {
            format!(
                "{}: correction {number} gives `{key}` an invalid value: {detail}",
                self.source
            )
        })?;
        if number == 0 {
            return self.fail("names a key outside any `[[correction]]`".to_owned());
        }
        let Some(current) = entries.get_mut(number - 1) else {
            return self.fail(format!("correction {number} is no entry"));
        };
        if in_fields {
            match current.fields.as_mut() {
                None => return self.fail(format!("correction {number} is no entry")),
                Some(fields) => {
                    if fields.insert(key.clone(), value).is_some() {
                        return self.fail(format!("correction {number} names field `{key}` twice"));
                    }
                }
            }
            return Ok(());
        }
        let slot = match key.as_str() {
            "package" => &mut current.package,
            "model" => &mut current.model,
            "source" => &mut current.source,
            "upstream" => &mut current.upstream,
            "fields" => {
                if current.fields.is_some() || current.fields_bad {
                    return self.fail(format!("correction {number} names `fields` twice"));
                }
                match value {
                    Value::Object(fields) => {
                        current.fields = Some(fields);
                        return Ok(());
                    }
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Array(_) => {
                        current.fields_bad = true;
                        return Ok(());
                    }
                }
            }
            _ => return self.fail(format!("correction {number} names unknown key `{key}`")),
        };
        if slot.is_some() {
            return self.fail(format!("correction {number} names `{key}` twice"));
        }
        *slot = Some(value);
        Ok(())
    }
}

/// Parses corrections `text` from `source`: one `[[correction]]` array
/// whose entries each name `package`, `model`, `fields`, `source` and
/// `upstream`. Anything else fails naming the file.
pub(crate) fn parse_corrections(source: &str, text: &str) -> Result<Vec<Correction>, String> {
    let mut parser = Parser {
        source,
        text,
        bytes: text.as_bytes(),
        pos: 0,
        entry: 0,
        fields: false,
    };
    let mut entries: Vec<RawEntry> = Vec::new();
    loop {
        parser.skip_gap();
        if parser.peek().is_none() {
            break;
        }
        if parser.starts("[[") {
            parser.array_header()?;
            entries.push(RawEntry::default());
            parser.entry = entries.len();
            parser.fields = false;
        } else if parser.peek() == Some(b'[') {
            parser.table_header(&mut entries)?;
        } else {
            parser.key_value(&mut entries)?;
        }
        // A top-level item ends at its newline: anything else on the
        // line is trailing text, never the next item.
        if parser.end_line("the value")? {
            break;
        }
    }
    let mut corrections = Vec::with_capacity(entries.len());
    for (index, raw) in entries.into_iter().enumerate() {
        corrections.push(finish_correction(source, index + 1, raw)?);
    }
    Ok(corrections)
}

/// One required string key of entry `number`: missing, empty and
/// non-string values all fail, so an empty page reads as no page.
fn correction_string(
    source: &str,
    number: usize,
    key: &str,
    value: Option<Value>,
) -> Result<String, String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Ok(text),
        None | Some(Value::String(_)) => {
            Err(format!("{source}: correction {number} names no `{key}`"))
        }
        Some(_) => Err(format!(
            "{source}: correction {number} names a non-string `{key}`"
        )),
    }
}

/// A parsed entry with every required key checked.
fn finish_correction(source: &str, number: usize, raw: RawEntry) -> Result<Correction, String> {
    let package = correction_string(source, number, "package", raw.package)?;
    let model = correction_string(source, number, "model", raw.model)?;
    let source_url = correction_string(source, number, "source", raw.source)?;
    let upstream = correction_string(source, number, "upstream", raw.upstream)?;
    if raw.fields_bad {
        return Err(format!(
            "{source}: correction {number} names a non-table `fields`"
        ));
    }
    match raw.fields {
        None => Err(format!("{source}: correction {number} names no `fields`")),
        Some(fields) if fields.is_empty() => {
            Err(format!("{source}: correction {number} names no fields"))
        }
        Some(fields) => Ok(Correction {
            package,
            model,
            fields,
            source: source_url,
            upstream,
        }),
    }
}

/// One `"`-quoted TOML string.
fn write_toml_string(value: &str, text: &mut String) {
    text.push('"');
    for next in value.chars() {
        match next {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{08}' => text.push_str("\\b"),
            '\u{0C}' => text.push_str("\\f"),
            next if next.is_control() => {
                text.push_str(&format!("\\u{:04X}", u32::from(next)));
            }
            _ => text.push(next),
        }
    }
    text.push('"');
}

/// One TOML value: only strings, integers, floats, booleans, arrays and
/// objects reach here, as the corrections reader parses nothing else.
/// A null writes the TOML string `"null"` so a later read fails loudly
/// instead of applying a silent wrong value.
fn write_toml_value(value: &Value, text: &mut String) {
    match value {
        Value::Null => text.push_str("\"null\""),
        Value::Bool(kept) => text.push_str(if *kept { "true" } else { "false" }),
        Value::Number(number) => text.push_str(&number.to_string()),
        Value::String(kept) => write_toml_string(kept, text),
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                write_toml_value(item, text);
            }
            text.push(']');
        }
        Value::Object(table) => {
            text.push('{');
            for (index, (key, item)) in table.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                text.push_str(key);
                text.push_str(" = ");
                write_toml_value(item, text);
            }
            text.push('}');
        }
    }
}

/// The corrections file text for `corrections`: the header comment plus
/// one `[[correction]]` entry each, so a prune rewrite keeps the file's
/// shape. An empty list writes the header alone.
pub(crate) fn render_corrections(corrections: &[Correction]) -> String {
    let mut text = String::from(
        "# Cited corrections to the generated model lists (`docs/model-routing.md`,\n\
         # \"What a provider extension declares\"): `cargo xtask models-dev` applies\n\
         # each entry after the per-package table and deletes an entry once\n\
         # models.dev agrees with it. Each entry names the vendor page that says\n\
         # so (`source`) and the models.dev pull request fixing it upstream\n\
         # (`upstream`). Only `name`, `protocol`, `context_window`,\n\
         # `max_output_tokens`, `input` and `cost` may be overridden; every other\n\
         # field comes from the per-package table alone.\n",
    );
    for correction in corrections {
        text.push('\n');
        text.push_str("[[correction]]\n");
        for (key, value) in [
            ("package", correction.package.as_str()),
            ("model", correction.model.as_str()),
            ("source", correction.source.as_str()),
            ("upstream", correction.upstream.as_str()),
        ] {
            text.push_str(key);
            text.push_str(" = ");
            write_toml_string(value, &mut text);
            text.push('\n');
        }
        text.push_str("[correction.fields]\n");
        for (key, value) in &correction.fields {
            text.push_str(key);
            text.push_str(" = ");
            write_toml_value(value, &mut text);
            text.push('\n');
        }
    }
    text
}

/// The file stem of the package's path: `opencode-zen` of
/// `providers/opencode/providers/opencode-zen.json`.
fn stem(package: &Package) -> &str {
    let file = package.path.rsplit('/').next().unwrap_or(package.path);
    file.strip_suffix(".json").unwrap_or(file)
}

/// Whether `name` is the stem or the full path of `package`.
pub(crate) fn names_package(package: &Package, name: &str) -> bool {
    name == package.path || name == stem(package)
}

/// Every correction names a known package once, overrides only derived
/// fields with values of their shape, and never moves the table-pinned
/// codex window.
pub(crate) fn check_corrections(
    packages: &[Package],
    corrections: &[Correction],
) -> Result<(), String> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for correction in corrections {
        let entry = format!(
            "correction for `{}/{}`",
            correction.package, correction.model
        );
        let Some(package) = packages
            .iter()
            .find(|package| names_package(package, &correction.package))
        else {
            return Err(format!(
                "{CORRECTIONS}: {entry} names unknown package `{}`",
                correction.package
            ));
        };
        if seen.contains(&(correction.package.as_str(), correction.model.as_str())) {
            return Err(format!("{CORRECTIONS}: {entry} appears twice"));
        }
        seen.push((correction.package.as_str(), correction.model.as_str()));
        for (field, value) in &correction.fields {
            if TABLE_ONLY_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "{CORRECTIONS}: {entry} overrides table-only field `{field}`"
                ));
            }
            if !OVERRIDABLE_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "{CORRECTIONS}: {entry} names unknown field `{field}`"
                ));
            }
            let shape = match field.as_str() {
                "name" | "protocol" => value.is_string(),
                "context_window" | "max_output_tokens" => value.as_u64().is_some(),
                "input" => value
                    .as_array()
                    .is_some_and(|input| input.iter().all(|kind| kind.is_string())),
                "cost" => value.is_object(),
                _ => false,
            };
            if !shape {
                let want = match field.as_str() {
                    "name" | "protocol" => "a non-string",
                    "context_window" | "max_output_tokens" => "a non-integer",
                    "input" => "a non-string array",
                    _ => "a non-object",
                };
                return Err(format!("{CORRECTIONS}: {entry} sets `{field}` to {want}"));
            }
        }
        if stem(package) == "codex" && correction.fields.contains_key("context_window") {
            return Err(format!(
                "{CORRECTIONS}: {entry} overrides the table-pinned `context_window`"
            ));
        }
    }
    Ok(())
}
