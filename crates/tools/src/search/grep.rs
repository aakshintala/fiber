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
use super::{Out, Outcome};

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
    buffered.flush().unwrap_or(());
    match outcome {
        Outcome::Done(code) => code,
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
    // reads standard input.
    let paths = if options.paths.is_empty() && options.recursive {
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
                let searched = search_stream(
                    &mut search,
                    &options,
                    label.as_deref(),
                    b"(standard input)",
                    stdin,
                    stderr,
                    &mut out,
                );
                failed |= searched.failed;
                matched |= searched.matched;
            }
            Job::File { read, show: shown } => {
                if !included(&options, &shown) {
                    continue;
                }
                match File::open(&read) {
                    Ok(file) => {
                        let label = labeled(&options, show, &shown);
                        let searched = search_stream(
                            &mut search,
                            &options,
                            label.as_deref(),
                            shown.as_os_str().as_encoded_bytes(),
                            &mut BufReader::new(file),
                            stderr,
                            &mut out,
                        );
                        failed |= searched.failed;
                        matched |= searched.matched;
                    }
                    Err(error) => {
                        complaint(stderr, &shown, &walk::io_message(&error));
                        failed = true;
                    }
                }
            }
            Job::Dir { walk } => {
                walked.push(walk.clone());
                for entry in walk::walk(cwd, &walk, None) {
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
                            let label = labeled(&options, show, &found.display);
                            let errors = found.display.as_os_str().as_encoded_bytes().to_vec();
                            match File::open(cwd.join(&found.display)) {
                                Ok(file) => {
                                    let searched = search_stream(
                                        &mut search,
                                        &options,
                                        label.as_deref(),
                                        &errors,
                                        &mut BufReader::new(file),
                                        stderr,
                                        &mut out,
                                    );
                                    failed |= searched.failed;
                                    matched |= searched.matched;
                                }
                                Err(error) => {
                                    complaint(stderr, &found.display, &walk::io_message(&error));
                                    failed = true;
                                }
                            }
                        }
                        Err(error) => {
                            complaint(stderr, &error.display, &error.message);
                            failed = true;
                        }
                    }
                }
            }
        }
    }
    out.flush();
    if !matched && !failed {
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
/// for a back-reference or a pattern ripgrep cannot compile.
fn compile(options: &Options) -> Result<Search, ()> {
    let source = std::str::from_utf8(&options.pattern).map_err(|_| ())?;
    let pattern = match options.mode {
        Mode::Basic => bre::translate_bre(source).ok_or(())?,
        Mode::Extended => bre::translate_ere(source).ok_or(())?,
        Mode::Fixed => source.to_owned(),
    };
    let mut matcher = RegexMatcherBuilder::new();
    matcher.case_insensitive(options.ignore_case);
    matcher.word(options.word);
    if options.mode == Mode::Fixed {
        matcher.fixed_strings(true);
    }
    let matcher = matcher.build(&pattern).map_err(|_| ())?;
    let mut searcher = SearcherBuilder::new();
    searcher.line_number(true);
    searcher.invert_match(options.invert);
    searcher.before_context(options.before);
    searcher.after_context(options.after);
    // A NUL byte in what is read skips the file silently, as `grep -I` does.
    searcher.binary_detection(BinaryDetection::quit(0));
    Ok(Search {
        matcher,
        searcher: searcher.build(),
    })
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
}

/// Searches one input, printing what the options ask for: the path, the
/// count, or the lines. `err_label` names the input when reading fails.
fn search_stream(
    search: &mut Search,
    options: &Options,
    label: Option<&[u8]>,
    err_label: &[u8],
    reader: &mut dyn Read,
    stderr: &mut dyn Write,
    out: &mut Out<'_>,
) -> Found {
    let mut lines = Lines { lines: Vec::new() };
    if let Err(error) = search
        .searcher
        .search_reader(&search.matcher, reader, &mut lines)
    {
        complaint_bytes(stderr, err_label, &walk::io_message(&error));
        return Found {
            matched: false,
            failed: true,
        };
    }
    if options.files_with_matches {
        let matched = lines
            .lines
            .iter()
            .any(|line| matches!(line, Line::Match { .. }));
        if matched {
            out.emit(label.unwrap_or_default());
            out.emit(b"\n");
        }
        return Found {
            matched,
            failed: false,
        };
    }
    if options.count {
        let count = lines
            .lines
            .iter()
            .filter(|line| matches!(line, Line::Match { .. }))
            .count();
        if let Some(label) = label {
            out.emit(label);
            out.emit(b":");
        }
        out.emit(count.to_string().as_bytes());
        out.emit(b"\n");
        return Found {
            matched: count > 0,
            failed: false,
        };
    }
    let mut matched = false;
    for line in &lines.lines {
        match line {
            Line::Match { no, text } => {
                matched = true;
                if let Some(label) = label {
                    out.emit(label);
                    out.emit(b":");
                }
                if options.line_numbers {
                    out.emit(no.to_string().as_bytes());
                    out.emit(b":");
                }
                out.emit(text);
                out.emit(b"\n");
            }
            Line::Context { no, text } => {
                if let Some(label) = label {
                    out.emit(label);
                    out.emit(b"-");
                }
                if options.line_numbers {
                    out.emit(no.to_string().as_bytes());
                    out.emit(b"-");
                }
                out.emit(text);
                out.emit(b"\n");
            }
            Line::Break => out.emit(b"--\n"),
        }
    }
    Found {
        matched,
        failed: false,
    }
}

/// The lines one search reported, in order.
struct Lines {
    /// The matches, context and breaks, in order.
    lines: Vec<Line>,
}

/// One reported line.
enum Line {
    /// A matching line: its number and bytes without the terminator.
    Match {
        /// The one-based line number.
        no: u64,
        /// The line without its terminator.
        text: Vec<u8>,
    },
    /// A context line: its number and bytes without the terminator.
    Context {
        /// The one-based line number.
        no: u64,
        /// The line without its terminator.
        text: Vec<u8>,
    },
    /// A break between context groups.
    Break,
}

impl Sink for Lines {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        self.lines.push(Line::Match {
            no: mat.line_number().unwrap_or_default(),
            text: stripped(mat.bytes()),
        });
        Ok(true)
    }

    fn context(&mut self, _: &Searcher, context: &SinkContext<'_>) -> Result<bool, std::io::Error> {
        self.lines.push(Line::Context {
            no: context.line_number().unwrap_or_default(),
            text: stripped(context.bytes()),
        });
        Ok(true)
    }

    fn context_break(&mut self, _: &Searcher) -> Result<bool, std::io::Error> {
        self.lines.push(Line::Break);
        Ok(true)
    }
}

/// The line without its terminator, keeping a carriage return as GNU does.
fn stripped(bytes: &[u8]) -> Vec<u8> {
    bytes.strip_suffix(b"\n").unwrap_or(bytes).to_vec()
}

/// Complains to standard error, as GNU does: `grep: <path>: <reason>`.
fn complaint(stderr: &mut dyn Write, path: &Path, message: &str) {
    stderr.write_all(b"grep: ").unwrap_or(());
    stderr
        .write_all(path.as_os_str().as_encoded_bytes())
        .unwrap_or(());
    writeln!(stderr, ": {message}").unwrap_or(());
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
