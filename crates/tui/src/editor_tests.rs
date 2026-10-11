//! Tests for Ctrl+G: choosing what opens, running the editor on a
//! temporary file, and applying what it returns.

use super::{NEXT, NO_EDITOR, Target, TempFile, command, open, run_in};
use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use contract::clock::Clock;
use fakes::Deadline;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// `n` numbered lines joined by line breaks.
fn lines(n: usize) -> String {
    (1..=n)
        .map(|at| format!("l{at}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn visual_wins_then_editor_and_empty_values_do_not_count() {
    let env = |visual: Option<&str>, editor: Option<&str>| {
        let visual = visual.map(str::to_owned);
        let editor = editor.map(str::to_owned);
        command(move |name| match name {
            "VISUAL" => visual.clone(),
            "EDITOR" => editor.clone(),
            _ => None,
        })
    };
    assert_eq!(env(Some("code -w"), Some("vi")), Some("code -w".to_owned()));
    assert_eq!(env(None, Some("vi")), Some("vi".to_owned()));
    assert_eq!(env(Some(""), Some("vi")), Some("vi".to_owned()));
    assert_eq!(env(Some(" "), Some("vi")), Some("vi".to_owned()));
    assert_eq!(env(None, Some("")), None);
    assert_eq!(env(None, None), None);
}

/// A fake editor in its own directory, and the directory the temporary
/// files go in. Dropping it kills any process still running its script.
struct Fake {
    /// Holds the script and what it copies out.
    dir: fakes::TempDir,
    /// Where the temporary files are made.
    temp: fakes::TempDir,
    /// The script, run through `/bin/sh`.
    script: PathBuf,
    watchdog: Option<fakes::Watchdog>,
}

impl Fake {
    /// A fake editor whose script is `body`; `$OUT` in it is the
    /// directory for what it copies out.
    fn new(body: &str) -> Self {
        let dir = fakes::TempDir::new("editor");
        let temp = fakes::TempDir::new("drafts");
        let script = dir.path().join("editor.sh");
        let body = body.replace("$OUT", &dir.path().display().to_string());
        std::fs::write(&script, body).unwrap_or_else(|err| panic!("script: {err}"));
        let watchdog = Some(fakes::Watchdog::matching(&script.display().to_string()));
        Self {
            dir,
            temp,
            script,
            watchdog,
        }
    }

    /// The editor command `/bin/sh <script>`, with `before` ahead of it and
    /// `after` behind it.
    fn command(&self, before: &str, after: &str) -> String {
        format!("{before}/bin/sh {}{after}", self.script.display())
    }

    /// Runs `command` on `text` with one deadline.
    #[track_caller]
    fn run_command(&self, command: String, text: &str) -> Result<String, String> {
        let temp = self.temp.path().to_path_buf();
        let text = text.to_owned();
        let (done, finished) = mpsc::channel();
        std::thread::Builder::new()
            .name("editor-run".to_owned())
            .spawn(move || done.send(run_in(&temp, &command, &text)).unwrap_or(()))
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        match Deadline::after(DEADLINE).recv(&finished) {
            Ok(result) => result,
            Err(err) => panic!("waited {DEADLINE:?} for the editor: {err}"),
        }
    }

    /// Runs the editor `/bin/sh <script>` on `text` with one deadline.
    #[track_caller]
    fn run(&self, text: &str) -> Result<String, String> {
        self.run_command(self.command("", ""), text)
    }

    /// The files left where the temporary files are made.
    fn left(&self) -> Vec<String> {
        names(self.temp.path())
    }

    /// The path of something the script copied out.
    fn out(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(DEADLINE);
        }
    }
}

/// The names in `dir`.
fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir: {err}"))
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_editor_gets_a_private_file_holding_the_text_and_its_edit_returns() {
    let fake = Fake::new(
        "cp -p \"$1\" $OUT/copy\nprintf '%s' \"$1\" > $OUT/path\nprintf 'new\\ntext\\n' > \"$1\"\n",
    );
    assert_eq!(fake.run("old text"), Ok("new\ntext".to_owned()));
    let copy = fake.out("copy");
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap_or_default(),
        "old text\n"
    );
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&copy)
            .unwrap_or_else(|err| panic!("metadata: {err}"))
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    let path = PathBuf::from(std::fs::read_to_string(fake.out("path")).unwrap_or_default());
    assert_eq!(path.parent(), Some(fake.temp.path()));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("fiber-draft-{}-", std::process::id());
    let n = name
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(".md"))
        .unwrap_or_else(|| panic!("{name}"));
    assert!(
        !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit()),
        "{name}"
    );
    assert!(fake.left().is_empty());
}

#[test]
fn a_file_left_as_it_was_returns_the_same_text() {
    let fake = Fake::new(":\n");
    for text in ["plain", "kept\n", "a\n", "a\n\n", ""] {
        assert_eq!(fake.run(text), Ok(text.to_owned()), "{text:?}");
    }
    assert!(fake.left().is_empty());
}

#[test]
fn one_trailing_line_break_is_dropped_from_the_edit() {
    // Written without one, the text returns as written.
    let fake = Fake::new("printf 'new' > \"$1\"\n");
    assert_eq!(fake.run("a\n"), Ok("new".to_owned()));
    // Only one is dropped.
    let fake = Fake::new("printf 'new\\n\\n' > \"$1\"\n");
    assert_eq!(fake.run("a"), Ok("new\n".to_owned()));
    // A line added at the end stays, whatever the text ended with.
    let fake = Fake::new("printf '\\n' >> \"$1\"\n");
    assert_eq!(fake.run("a"), Ok("a\n".to_owned()));
    assert_eq!(fake.run("a\n"), Ok("a\n\n".to_owned()));
    assert!(fake.left().is_empty());
}

#[test]
fn a_command_with_arguments_gets_the_file_last() {
    let fake = Fake::new("printf '%s:%s' \"$1\" \"$(cat \"$2\")\" > \"$2\"\n");
    let command = fake.command("", " --wait");
    assert_eq!(fake.run_command(command, "x"), Ok("--wait:x".to_owned()));
}

#[test]
fn a_non_zero_exit_keeps_the_draft_and_names_the_status() {
    let fake = Fake::new("printf 'changed' > \"$1\"\nexit 3\n");
    assert_eq!(
        fake.run("text"),
        Err("The editor exited with status 3; the draft is unchanged.".to_owned())
    );
    assert!(fake.left().is_empty());
}

#[test]
fn an_editor_ended_by_a_signal_keeps_the_draft_and_names_it() {
    // `exec` makes the script the process Fiber waits on, as an editor run
    // by its own name is, so the signal ends that process.
    let fake = Fake::new("printf 'changed' > \"$1\"\nkill -INT $$\n");
    let command = fake.command("exec ", "");
    assert_eq!(
        fake.run_command(command, "text"),
        Err("The editor was ended by signal 2; the draft is unchanged.".to_owned())
    );
    assert!(fake.left().is_empty());
}

#[test]
fn an_unreadable_file_keeps_the_draft_and_says_so() {
    let fake = Fake::new("rm \"$1\"\n");
    let result = fake.run("text");
    assert!(
        result
            .as_ref()
            .is_err_and(|err| err.starts_with("Could not read the edited file: ")),
        "{result:?}"
    );
    assert!(fake.left().is_empty());
}

impl Fake {
    /// Runs `open` with the editor on `path`, with one deadline.
    #[track_caller]
    fn open(&self, path: &Path) -> Result<(), String> {
        let command = self.command("", "");
        let path = path.to_path_buf();
        let (done, finished) = mpsc::channel();
        std::thread::Builder::new()
            .name("editor-open".to_owned())
            .spawn(move || done.send(open(&command, &path)).unwrap_or(()))
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        match Deadline::after(DEADLINE).recv(&finished) {
            Ok(result) => result,
            Err(err) => panic!("waited {DEADLINE:?} for the editor: {err}"),
        }
    }
}

#[test]
fn open_runs_the_command_on_the_path_even_when_it_does_not_exist() {
    let fake = Fake::new("printf 'opened' >> \"$1\"\n");
    let path = fake.temp.path().join("config.json");
    assert_eq!(fake.open(&path), Ok(()));
    assert_eq!(
        std::fs::read_to_string(&path).ok(),
        Some("opened".to_owned())
    );
    // No temporary file is made beside it.
    assert_eq!(fake.left(), ["config.json"]);
}

#[test]
fn open_reports_a_failing_editor() {
    let fake = Fake::new("exit 3\n");
    let path = fake.temp.path().join("config.json");
    assert_eq!(
        fake.open(&path),
        Err("The editor exited with status 3.".to_owned())
    );
    let fake = Fake::new("kill -INT $$\n");
    let command = fake.command("exec ", "");
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("editor-open-signal".to_owned())
        .spawn(move || done.send(open(&command, &path)).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&finished).ok(),
        Some(Err("The editor was ended by signal 2.".to_owned()))
    );
}

#[test]
fn no_directory_for_the_file_says_so_and_runs_nothing() {
    let fake = Fake::new("printf 'ran' > $OUT/ran\n");
    let missing = fake.temp.path().join("missing");
    let result = run_in(&missing, &fake.command("", ""), "x");
    assert!(
        result
            .as_ref()
            .is_err_and(|err| err.starts_with("Could not write a file for the editor: ")),
        "{result:?}"
    );
    assert!(!fake.out("ran").exists());
}

/// An app in `/w`, 80 columns wide.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

#[test]
fn ctrl_g_opens_the_token_beside_the_cursor_or_the_whole_draft() {
    let mut app = app();
    for ch in "ab".chars() {
        press(&mut app, Key::Char(ch));
    }
    app.on_edit(Edit::Paste(lines(11)));
    app.on_edit(Edit::Paste(lines(12)));
    // After #2.
    assert_eq!(
        press(&mut app, Key::CtrlG),
        Effect::Editor {
            target: Target::Token(2),
            text: lines(12)
        }
    );
    // Between #1 and #2: the one before.
    app.on_edit(Edit::Left);
    assert_eq!(
        press(&mut app, Key::CtrlG),
        Effect::Editor {
            target: Target::Token(1),
            text: lines(11)
        }
    );
    // Between `a` and `b`: the whole draft, tokens expanded.
    app.on_edit(Edit::Left);
    app.on_edit(Edit::Left);
    assert_eq!(
        press(&mut app, Key::CtrlG),
        Effect::Editor {
            target: Target::Draft,
            text: format!("ab{}{}", lines(11), lines(12))
        }
    );
}

#[test]
fn the_whole_draft_comes_back_as_typed_with_the_cursor_at_its_end() {
    let mut app = app();
    app.on_edit(Edit::Paste(lines(11)));
    app.editor_returned(Target::Draft, Ok(format!("{}\r\nend", lines(11))));
    assert_eq!(app.draft(), format!("{}\nend", lines(11)));
    // No token: every line is a row, and the cursor is after `end`.
    assert_eq!(app.input().rows(80).len(), 12);
    assert_eq!(app.input().position(), app.draft().chars().count());
}

#[test]
fn a_tokens_text_comes_back_to_that_token() {
    let mut app = app();
    press(&mut app, Key::Char('a'));
    app.on_edit(Edit::Paste(lines(11)));
    app.editor_returned(Target::Token(1), Ok(lines(13)));
    assert_eq!(app.draft(), format!("a{}", lines(13)));
    assert_eq!(app.input().rows(80), vec!["› a[Pasted text #1 · 13 lines]"]);
}

#[test]
fn an_error_is_the_notice_and_the_draft_stays() {
    let mut app = app();
    press(&mut app, Key::Char('a'));
    app.editor_returned(Target::Draft, Err(NO_EDITOR.to_owned()));
    assert_eq!(app.notice(), Some(NO_EDITOR));
    assert_eq!(app.draft(), "a");
}

#[test]
fn ctrl_g_does_nothing_while_the_search_panel_is_open() {
    let mut app = app();
    press(&mut app, Key::CtrlR);
    assert_eq!(press(&mut app, Key::CtrlG), Effect::None);
    assert!(app.completions().is_some());
}

/// The name [`TempFile::create`] gives number `n` in `dir`.
fn draft_name(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("fiber-draft-{}-{n}.md", std::process::id()))
}

#[test]
fn a_taken_name_moves_on_to_the_next() {
    let temp = fakes::TempDir::new("drafts");
    let n = NEXT.load(std::sync::atomic::Ordering::Relaxed);
    for taken in [n, n + 1] {
        std::fs::write(draft_name(temp.path(), taken), "taken")
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
    let (file, _) = TempFile::create(temp.path()).unwrap_or_else(|err| panic!("create: {err}"));
    assert_eq!(file.0, draft_name(temp.path(), n + 2));
    // The files already there are left as they were.
    for taken in [n, n + 1] {
        let text = std::fs::read_to_string(draft_name(temp.path(), taken)).unwrap_or_default();
        assert_eq!(text, "taken");
    }
    drop(file);
    assert!(!draft_name(temp.path(), n + 2).exists());
}

#[test]
fn every_name_taken_or_another_error_gives_up() {
    let temp = fakes::TempDir::new("drafts");
    let n = NEXT.load(std::sync::atomic::Ordering::Relaxed);
    for taken in n..n + 16 {
        std::fs::write(draft_name(temp.path(), taken), "taken")
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
    let kind = TempFile::create(temp.path()).err().map(|err| err.kind());
    assert_eq!(kind, Some(std::io::ErrorKind::AlreadyExists));
    // Any other error stops at the first name, trying no more.
    let n = NEXT.load(std::sync::atomic::Ordering::Relaxed);
    let kind = TempFile::create(&temp.path().join("missing"))
        .err()
        .map(|err| err.kind());
    assert_eq!(kind, Some(std::io::ErrorKind::NotFound));
    assert_eq!(NEXT.load(std::sync::atomic::Ordering::Relaxed), n + 1);
}
