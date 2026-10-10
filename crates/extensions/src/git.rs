//! Names, and fetching with the system `git` (`docs/extensions.md`,
//! "Names"). Fiber runs `git` as a person would, so their SSH keys and
//! credential helpers apply.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use contract::clock::Clock;

use crate::Error;
use crate::host::exec;

pub(crate) use config::short_name;
pub use config::{SHORT_NAMES, full_name};

/// How long each `git` call may run before it is stopped
/// (`docs/extensions.md`, "Installing").
pub(crate) const GIT_DEADLINE: Duration = Duration::from_secs(120);

/// Whether `typed` names a directory rather than an extension.
pub fn is_path(typed: &str) -> bool {
    typed.starts_with(['.', '/', '~']) || Path::new(typed).join("extension.json").is_file()
}

/// A name as its repository and the directory inside it
/// (`docs/extensions.md`, "Names").
///
/// Without a `.git` marker, the first three segments are the repository
/// (`github.com/owner/repo`). A segment that is `<x>.git`, with `<x>`
/// non-empty, marks where the repository ends, so a repository inside
/// subgroups can be named. The marker stays on the repository string.
pub(crate) fn split(name: &str) -> Result<(&str, &str), Error> {
    let end = git_marker_end(name)
        .or_else(|| name.match_indices('/').nth(2).map(|(i, _)| i))
        .unwrap_or(name.len());
    let (repo, rest) = name.split_at(end);
    let dir = rest.strip_prefix('/').unwrap_or(rest);
    // A marked repository needs at least three segments; an unmarked one
    // is exactly the first three, so fewer than three is the same refusal.
    if repo.split('/').count() < 3 || repo.split('/').any(str::is_empty) {
        return Err(Error::BadName { name: name.into() });
    }
    Ok((repo, dir))
}

/// The byte index just past the first `<x>.git` segment, when the name has one.
fn git_marker_end(name: &str) -> Option<usize> {
    let mut end = 0;
    for segment in name.split('/') {
        end += segment.len();
        if segment.len() > ".git".len() && segment.ends_with(".git") {
            return Some(end);
        }
        end += 1;
    }
    None
}

/// A `git` call stopped at its deadline, as a fetch failure naming the call.
fn timeout(command: &str) -> Error {
    Error::Git {
        command: command.into(),
        why: timeout_why(),
    }
}

/// Why a call stopped at its deadline names the deadline's seconds.
fn timeout_why() -> String {
    format!(
        "did not finish within {} s, so it was stopped",
        GIT_DEADLINE.as_secs()
    )
}

/// Whether `err` is a `git` call stopped at its deadline.
fn is_timeout(err: &Error) -> bool {
    matches!(err, Error::Git { why, .. } if *why == timeout_why())
}

/// Whether `ls-remote`'s stderr means the repository is not there.
/// "Could not read from remote repository" is not enough: an SSH
/// authentication failure prints it too, and a host that asks for
/// credentials cannot be told apart from a missing repository.
fn repository_missing(why: &str) -> bool {
    let why = why.to_lowercase();
    why.contains("not found") || why.contains("does not appear to be a git repository")
}

/// Where extensions are fetched from.
pub struct Origin {
    git: String,
    url: Box<dyn Fn(&str) -> String>,
}

impl Origin {
    /// `git` fetching `https://<repository>`.
    pub fn github() -> Self {
        Self::new("git", |repo| format!("https://{repo}"))
    }

    /// The `git` program, and the URL a repository name is fetched from.
    pub fn new(git: impl Into<String>, url: impl Fn(&str) -> String + 'static) -> Self {
        Self {
            git: git.into(),
            url: Box::new(url),
        }
    }

    /// Runs `git` with `args` in `dir`, stopped at [`GIT_DEADLINE`] on
    /// `clock`. The call stays in Fiber's process group, so SSH and
    /// credential-helper prompts on the terminal still work. Without `dir`
    /// the working directory is inherited.
    fn run(&self, args: &[&str], dir: Option<&Path>, clock: &dyn Clock) -> Result<String, Error> {
        let req = exec::ExecRequest {
            program: self.git.clone(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: dir.map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf),
            // `git`'s output is not capped today.
            cap: usize::MAX,
            // In Fiber's process group, so SSH and credential-helper
            // prompts on the terminal still work as they do today.
            own_group: false,
        };
        let deadline = clock.now().checked_add(GIT_DEADLINE).unwrap_or(clock.now());
        // Never cancelled except by the call's own end: the sender drops
        // when this returns.
        let (_cancel, cancel) = mpsc::channel::<()>();
        let command = args.join(" ");
        match exec::run(&req, clock, Some(deadline), cancel) {
            Err(failed) => Err(match failed.source {
                Some(source) if source.kind() == std::io::ErrorKind::NotFound => Error::GitMissing,
                Some(source) => Error::Git {
                    command,
                    why: source.to_string(),
                },
                // After the spawn: a reader thread that could not start.
                None => Error::Git {
                    command,
                    why: failed.message,
                },
            }),
            Ok(ran) if ran.timed_out => Err(timeout(&command)),
            Ok(ran) => {
                if ran.exit_code != Some(0) {
                    return Err(Error::Git {
                        command,
                        why: String::from_utf8_lossy(&ran.stderr).trim().to_owned(),
                    });
                }
                Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
            }
        }
    }

    /// The repository's tags.
    pub(crate) fn tags(&self, repo: &str, clock: &dyn Clock) -> Result<Vec<String>, Error> {
        let url = (self.url)(repo);
        let out = match self.run(&["ls-remote", "--tags", "--refs", &url], None, clock) {
            Ok(out) => out,
            Err(Error::Git { why, .. }) if repository_missing(&why) => {
                return Err(Error::NoRepository {
                    name: repo.into(),
                    why,
                });
            }
            Err(err) => return Err(err),
        };
        Ok(out
            .lines()
            .filter_map(|l| l.split_once("refs/tags/").map(|(_, t)| t.to_owned()))
            .collect())
    }

    /// Clones the repository at `tag` to
    /// `dest`, history included only when `history`, and returns the exact
    /// commit. `dest/.git` stays.
    pub(crate) fn clone(
        &self,
        repo: &str,
        tag: &str,
        history: bool,
        dest: &Path,
        clock: &dyn Clock,
    ) -> Result<String, Error> {
        let url = (self.url)(repo);
        let to = dest.to_string_lossy();
        let mut args = vec!["clone", "--quiet"];
        if !history {
            args.extend(["--depth", "1"]);
        }
        args.extend(["--branch", tag]);
        args.extend([url.as_str(), &to]);
        self.run(&args, None, clock)?;
        Ok(self
            .run(&["rev-parse", "HEAD"], Some(dest), clock)?
            .trim()
            .to_owned())
    }

    /// What changed in `dir` of the clone since commit `old`. A call stopped
    /// at its deadline fails the update: an update never commits a change
    /// list the call did not finish reading.
    pub(crate) fn changes(
        &self,
        clone: &Path,
        old: &str,
        dir: &str,
        clock: &dyn Clock,
    ) -> Result<String, Error> {
        let dir = if dir.is_empty() { "." } else { dir };
        match self.run(
            &["diff", "--stat", old, "HEAD", "--", dir],
            Some(clone),
            clock,
        ) {
            Ok(out) => Ok(out),
            // The deadline stopped the call, so there is no change list to
            // commit; anything else still reads as an installed commit the
            // repository no longer holds.
            Err(err) if is_timeout(&err) => Err(err),
            Err(_) => Ok("The installed commit is not in the repository.\n".into()),
        }
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
