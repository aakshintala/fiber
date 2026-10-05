//! The `grep` search: matching lines in GNU's shape (`docs/tools.md`, "Search").

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};

use super::bre;
use super::fallback;
use super::find::{basename, glob_match};
use super::grep_args::{self, Mode, Options};
use super::notice;
use super::walk::{self, DirRoot, Root};
use super::{Out, Outcome, flush_exit};

/// Runs `grep` against the process's working directory and standard streams,
/// returning the exit code: 0 when something matched, 1 when nothing did, 2
/// on an error. A call the built-in does not handle replaces the process
/// with the system `grep` and never returns on success.
pub fn grep_main(args: Vec<OsString>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut buffered = std::io::BufWriter::new(stdout.lock());
    let mut stderr = std::io::stderr().lock();
    let outcome = run(&cwd, &args, &mut input, &mut buffered, &mut stderr);
    let flushed = buffered.flush();
    match outcome {
        Outcome::Done(code) => flush_exit("grep", code, flushed, &mut stderr),
        Outcome::Fallback => fallback::exec("grep", &args, &mut stderr),
    }
}

/// Runs `grep` against `cwd`, reading the filter from `stdin` and writing
/// matches to `stdout` and complaints to `stderr`, without touching the
/// process's own streams. `Outcome::Fallback` is returned before the first
/// byte is read from `stdin` or written to `stdout`.
pub(crate) fn run(
    cwd: &Path,
    args: &[OsString],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Outcome {
    let options = match grep_args::parse(args) {
        grep_args::Parsed::Run(options) => options,
        grep_args::Parsed::Fallback => return Outcome::Fallback,
        grep_args::Parsed::Error(message) => {
            writeln!(stderr, "{message}").unwrap_or(());
            return Outcome::Done(2);
        }
    };
    // Provisional ruling 20 on #298: `-o` is not built in until the owner
    // rules. A call with `-o` runs the system grep, as any other unhandled
    // flag does. Removing this branch and printing each match is the
    // follow-up.
    if options.only_matching {
        return Outcome::Fallback;
    }
    if options.files_with_matches && options.count {
        return Outcome::Fallback;
    }
    let mut search = match compile(&options) {
        Ok(search) => search,
        Err(()) => return Outcome::Fallback,
    };
    // `-r` with no path searches `.`; without `-r` and no path the filter
    // reads standard input. An implicit root prints without the walk's
    // `./` prefix (ruling 9 on #298); an explicit `.` keeps it.
    let implicit = options.paths.is_empty() && options.recursive;
    let paths = if implicit {
        vec![PathBuf::from(".")]
    } else {
        options.paths.clone()
    };
    // The path shows when more than one file is searched, or when `-r`
    // names a directory.
    let show = paths.len() > 1
        || (options.recursive
            && paths
                .iter()
                .any(|path| matches!(walk::root_of(cwd, path), Ok(Root::Dir(_)))));
    let mut out = Out::new(stdout);
    let mut failed = false;
    let mut matched = false;
    // Whether an earlier input already printed lines: with context, a
    // `--` separates this input's lines from those, as GNU does.
    let mut preceded = false;
    let mut walked = Vec::new();
    for job in jobs(cwd, &paths, &options, show) {
        if out.broken() {
            break;
        }
        match job {
            Job::Complaint { path, message } => {
                complaint_bytes(stderr, &path, &message);
                failed = true;
            }
            Job::Standard { label } => {
                let target = Target {
                    label: label.as_deref(),
                    preceded,
                };
                let searched = search_stream(
                    &mut search,
                    &options,
                    target,
                    b"(standard input)",
                    stdin,
                    stderr,
                    &mut out,
                );
                failed |= searched.failed;
                matched |= searched.matched;
                preceded |= searched.printed;
            }
            Job::File { read, show: shown } => {
                if !included(&options, &shown) {
                    continue;
                }
                let label = labeled(&options, show, &shown);
                let target = Target {
                    label: label.as_deref(),
                    preceded,
                };
                let searched = search_file(
                    &mut search,
                    &options,
                    &read,
                    &shown,
                    target,
                    stderr,
                    &mut out,
                );
                failed |= searched.failed;
                matched |= searched.matched;
                preceded |= searched.printed;
            }
            Job::Dir { walk } => {
                walked.push(walk.clone());
                for entry in walk::walk(cwd, &walk, None) {
                    if out.broken() {
                        break;
                    }
                    match entry {
                        Ok(found) => {
                            if found.depth == 0
                                || found.file_type.is_dir()
                                || found.file_type.is_symlink()
                            {
                                continue;
                            }
                            if !included(&options, &found.display) {
                                continue;
                            }
                            let label = labeled(&options, show, &found.display)
                                .map(|label| strip_implicit(label, implicit));
                            let target = Target {
                                label: label.as_deref(),
                                preceded,
                            };
                            let searched = search_file(
                                &mut search,
                                &options,
                                &cwd.join(&found.display),
                                &found.display,
                                target,
                                stderr,
                                &mut out,
                            );
                            failed |= searched.failed;
                            matched |= searched.matched;
                            preceded |= searched.printed;
                            // Each searched file's output is flushed before
                            // the next begins, so a pipe sees it promptly.
                            out.flush();
                        }
                        Err(error) => {
                            complaint(stderr, &error.display, &error.message);
                            failed = true;
                        }
                    }
                }
            }
        }
        // Each file's output is flushed before the next begins.
        out.flush();
    }
    if let Some(error) = out.take_error() {
        writeln!(stderr, "grep: writing output: {error}").unwrap_or(());
        failed = true;
    }
    if !out.broken() && !matched && !failed {
        // Computed only on no match: the second walk costs nothing
        // otherwise.
        let mut skipped = Vec::new();
        for dir in &walked {
            skipped = notice::union(skipped, notice::skipped(cwd, &dir.walk));
        }
        let pattern = String::from_utf8_lossy(&options.pattern);
        if let Some(line) = notice::grep_line(&pattern, &skipped) {
            writeln!(stderr, "{line}").unwrap_or(());
        }
    }
    Outcome::Done(if failed {
        2
    } else if matched {
        0
    } else {
        1
    })
}

/// The compiled search: one matcher and one line searcher.
struct Search {
    /// The pattern, compiled.
    matcher: grep_regex::RegexMatcher,
    /// The line searcher: numbers, inversion, context and binary skips.
    searcher: Searcher,
}

/// Compiles the pattern: nothing when the pattern needs the system grep, as
/// for a back-reference or a pattern ripgrep cannot compile. A pattern
/// with newlines reads as one alternative per line, as repeated `-e`
/// would: each line is translated on its own.
fn compile(options: &Options) -> Result<Search, ()> {
    let source = std::str::from_utf8(&options.pattern).map_err(|_| ())?;
    let mut pattern = String::new();
    for (index, part) in source.split('\n').enumerate() {
        if index > 0 {
            pattern.push('|');
        }
        match options.mode {
            Mode::Basic => pattern.push_str(&bre::translate_bre(part).ok_or(())?),
            Mode::Extended => pattern.push_str(&bre::translate_ere(part).ok_or(())?),
            Mode::Fixed => pattern.push_str(&escape_fixed(part)),
        }
    }
    let mut matcher = RegexMatcherBuilder::new();
    // Bytes, as GNU in the C locale: `.` matches any byte but a newline,
    // and `-i` folds ASCII only. The differential tests run the system
    // grep under `LC_ALL=C`.
    matcher.unicode(false);
    matcher.case_insensitive(options.ignore_case);
    matcher.word(options.word);
    let matcher = matcher.build(&pattern).map_err(|_| ())?;
    let mut searcher = SearcherBuilder::new();
    searcher.line_number(true);
    searcher.invert_match(options.invert);
    searcher.before_context(options.before);
    searcher.after_context(options.after);
    // Raw bytes, as GNU in the C locale: no BOM sniffing, so a UTF-8
    // BOM stays part of the line and UTF-16 bytes are searched as they
    // read instead of decoded. No encoding is set anywhere.
    searcher.bom_sniffing(false);
    // A NUL byte in what is read skips the file silently, as `grep -I` does.
    searcher.binary_detection(BinaryDetection::quit(0));
    Ok(Search {
        matcher,
        searcher: searcher.build(),
    })
}

/// Escapes a fixed string as a regular expression: every character that
/// reads as syntax gets a backslash, everything else passes through.
fn escape_fixed(pattern: &str) -> String {
    let mut escaped = String::with_capacity(pattern.len());
    for current in pattern.chars() {
        if matches!(
            current,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(current);
    }
    escaped
}

/// One thing to search, or one complaint, in the order given.
enum Job {
    /// Read the filter, labelled when the path shows.
    Standard {
        /// The label, or nothing unlabelled.
        label: Option<Vec<u8>>,
    },
    /// Read the file, printed as given.
    File {
        /// The file read.
        read: PathBuf,
        /// The path printed.
        show: PathBuf,
    },
    /// Walk the directory.
    Dir {
        /// The directory walked.
        walk: DirRoot,
    },
    /// Complain: the path as given and the reason.
    Complaint {
        /// The path as given.
        path: Vec<u8>,
        /// The reason.
        message: String,
    },
}

/// Sorts the paths into jobs: `-` reads the filter, files read themselves,
/// directories walk with `-r` and complain without it.
fn jobs(cwd: &Path, paths: &[PathBuf], options: &Options, show: bool) -> Vec<Job> {
    if paths.is_empty() {
        return vec![Job::Standard { label: None }];
    }
    let mut out = Vec::new();
    for path in paths {
        if path.as_os_str().as_encoded_bytes() == b"-" {
            out.push(Job::Standard {
                label: show.then(|| b"(standard input)".to_vec()),
            });
            continue;
        }
        match walk::root_of(cwd, path) {
            Err(error) => out.push(Job::Complaint {
                path: path.as_os_str().as_encoded_bytes().to_vec(),
                message: walk::io_message(&error),
            }),
            Ok(Root::File(file)) => out.push(Job::File {
                read: file.read,
                show: file.show,
            }),
            Ok(Root::Dir(dir)) => {
                if options.recursive {
                    out.push(Job::Dir { walk: dir });
                } else {
                    out.push(Job::Complaint {
                        path: path.as_os_str().as_encoded_bytes().to_vec(),
                        message: "Is a directory".to_owned(),
                    });
                }
            }
        }
    }
    out
}

/// The label a match prints under: the display path, or `(standard input)`.
/// `-l` always labels; otherwise only when the path shows.
fn labeled(options: &Options, show: bool, display: &Path) -> Option<Vec<u8>> {
    if options.files_with_matches || show {
        Some(display.as_os_str().as_encoded_bytes().to_vec())
    } else {
        None
    }
}

/// Strips the walk's `./` prefix below an implicit `.` root, so `grep -r`
/// with no path prints `a.txt` while an explicit `.` keeps `./a.txt`.
/// Anything without the prefix passes through.
fn strip_implicit(mut label: Vec<u8>, implicit: bool) -> Vec<u8> {
    if implicit && let Some(rest) = label.strip_prefix(b"./".as_slice()) {
        label = rest.to_vec();
    }
    label
}

/// Whether the file passes `--include` and `--exclude`: an exclusion wins,
/// an empty include list keeps all.
fn included(options: &Options, display: &Path) -> bool {
    let base = basename(display);
    if options
        .excludes
        .iter()
        .any(|glob| glob_match(glob, base, false))
    {
        return false;
    }
    options.includes.is_empty()
        || options
            .includes
            .iter()
            .any(|glob| glob_match(glob, base, false))
}

/// What searching one input found.
struct Found {
    /// Whether any line matched.
    matched: bool,
    /// Whether reading failed.
    failed: bool,
    /// Whether any line printed, for the `--` between inputs.
    printed: bool,
}

/// One input to search: what its lines print under, and whether an
/// earlier input already printed lines (for the `--` between inputs in
/// context mode).
#[derive(Clone, Copy)]
struct Target<'a> {
    /// The label lines print under, or nothing unlabelled.
    label: Option<&'a [u8]>,
    /// Whether an earlier input already printed lines.
    preceded: bool,
}

/// Opens `read` and searches it, printing as `target` says: one helper
/// for a named file and a walked one. `shown` names the input when
/// opening or reading it fails.
fn search_file(
    search: &mut Search,
    options: &Options,
    read: &Path,
    shown: &Path,
    target: Target<'_>,
    stderr: &mut dyn Write,
    out: &mut Out<'_>,
) -> Found {
    match File::open(read) {
        Ok(file) => {
            let errors = shown.as_os_str().as_encoded_bytes();
            search_stream(
                search,
                options,
                target,
                errors,
                &mut BufReader::new(file),
                stderr,
                out,
            )
        }
        Err(error) => {
            complaint(stderr, shown, &walk::io_message(&error));
            Found {
                matched: false,
                failed: true,
                printed: false,
            }
        }
    }
}

/// Searches one input, printing what the options ask for: the path, the
/// count, or each line as it matches, so a filter prints before the input
/// ends. `err_label` names the input when reading fails. `preceded` is
/// whether an earlier input already printed lines: with context, a `--`
/// separates this input's lines from those.
fn search_stream(
    search: &mut Search,
    options: &Options,
    target: Target<'_>,
    err_label: &[u8],
    reader: &mut dyn Read,
    stderr: &mut dyn Write,
    out: &mut Out<'_>,
) -> Found {
    let mut emit = Emit {
        options,
        label: target.label,
        out,
        matched: false,
        count: 0,
        listed: false,
        printed: false,
        prefix: target.preceded && (options.before > 0 || options.after > 0),
    };
    if let Err(error) = search
        .searcher
        .search_reader(&search.matcher, reader, &mut emit)
    {
        complaint_bytes(stderr, err_label, &walk::io_message(&error));
        return Found {
            matched: false,
            failed: true,
            printed: false,
        };
    }
    if options.files_with_matches {
        return Found {
            matched: emit.matched,
            failed: false,
            printed: false,
        };
    }
    if options.count {
        if let Some(label) = emit.label {
            emit.out.emit(label);
            emit.out.emit(b":");
        }
        let count = emit.count.to_string();
        emit.out.emit(count.as_bytes());
        emit.out.emit(b"\n");
        return Found {
            matched: emit.count > 0,
            failed: false,
            printed: false,
        };
    }
    Found {
        matched: emit.matched,
        failed: false,
        printed: emit.printed,
    }
}

/// What one search prints, line by line as the searcher reports it: the
/// sink writes through instead of collecting, so a filter prints before
/// the input ends and a closed pipe stops the search.
struct Emit<'a, 'w> {
    /// What was asked for.
    options: &'a Options,
    /// The label lines print under, or nothing unlabelled.
    label: Option<&'a [u8]>,
    /// Where lines go.
    out: &'a mut Out<'w>,
    /// Whether any line matched.
    matched: bool,
    /// How many lines matched, for `-c`.
    count: usize,
    /// Whether the `-l` label already printed: the search stops at the
    /// first match, so this only says it printed.
    listed: bool,
    /// Whether any line printed, for the `--` between inputs.
    printed: bool,
    /// Whether `--` separates this input's first line from an earlier one.
    prefix: bool,
}

impl Sink for Emit<'_, '_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        self.matched = true;
        if self.options.files_with_matches {
            if !self.listed {
                self.listed = true;
                self.out.emit(self.label.unwrap_or(b"(standard input)"));
                self.out.emit(b"\n");
            }
            // The label is printed: nothing more to learn.
            return Ok(false);
        }
        if self.options.count {
            self.count += 1;
            return Ok(!self.out.broken());
        }
        let text = stripped(mat.bytes());
        let no = mat.line_number().unwrap_or_default();
        self.line(no, &text, b':');
        Ok(!self.out.broken())
    }

    fn context(&mut self, _: &Searcher, context: &SinkContext<'_>) -> Result<bool, std::io::Error> {
        if self.options.files_with_matches || self.options.count {
            return Ok(true);
        }
        let text = stripped(context.bytes());
        let no = context.line_number().unwrap_or_default();
        self.line(no, &text, b'-');
        Ok(!self.out.broken())
    }

    fn context_break(&mut self, _: &Searcher) -> Result<bool, std::io::Error> {
        if self.options.files_with_matches || self.options.count {
            return Ok(true);
        }
        self.prefix = false;
        self.printed = true;
        self.out.emit(b"--\n");
        Ok(!self.out.broken())
    }
}

impl Emit<'_, '_> {
    /// Prints one match or context line: the label, the number and the
    /// text, joined by `sep` (`:` for a match, `-` for context).
    fn line(&mut self, no: u64, text: &[u8], sep: u8) {
        self.separate();
        if let Some(label) = self.label {
            self.out.emit(label);
            self.out.emit(&[sep]);
        }
        if self.options.line_numbers {
            self.out.emit(no.to_string().as_bytes());
            self.out.emit(&[sep]);
        }
        self.out.emit(text);
        self.out.emit(b"\n");
    }

    /// Starts this input's lines: the `--` separating them from an earlier
    /// input's, once, in context mode.
    fn separate(&mut self) {
        if self.prefix {
            self.prefix = false;
            self.out.emit(b"--\n");
        }
        self.printed = true;
    }
}

/// The line without its terminator, keeping a carriage return as GNU does.
fn stripped(bytes: &[u8]) -> Vec<u8> {
    bytes.strip_suffix(b"\n").unwrap_or(bytes).to_vec()
}

/// Complains to standard error, as GNU does: `grep: <path>: <reason>`.
fn complaint(stderr: &mut dyn Write, path: &Path, message: &str) {
    complaint_bytes(stderr, path.as_os_str().as_encoded_bytes(), message);
}

/// Complains with an already built path.
fn complaint_bytes(stderr: &mut dyn Write, path: &[u8], message: &str) {
    stderr.write_all(b"grep: ").unwrap_or(());
    stderr.write_all(path).unwrap_or(());
    writeln!(stderr, ": {message}").unwrap_or(());
}

#[cfg(test)]
#[path = "grep_tests.rs"]
mod tests;
