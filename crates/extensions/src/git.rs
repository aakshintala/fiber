//! Names, and fetching with the system `git` (`docs/extensions.md`,
//! "Names"). Fiber runs `git` as a person would, so their SSH keys and
//! credential helpers apply.

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Error;

pub(crate) use config::short_name;
pub use config::{SHORT_NAMES, full_name};

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

    fn run(&self, args: &[&str], dir: Option<&Path>) -> Result<String, Error> {
        let mut command = Command::new(&self.git);
        command.args(args).stdin(Stdio::null());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let out = command.output().map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                Error::GitMissing
            } else {
                Error::Git {
                    command: args.join(" "),
                    why: e.to_string(),
                }
            }
        })?;
        if !out.status.success() {
            return Err(Error::Git {
                command: args.join(" "),
                why: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The repository's tags.
    pub(crate) fn tags(&self, repo: &str) -> Result<Vec<String>, Error> {
        let url = (self.url)(repo);
        let out = match self.run(&["ls-remote", "--tags", "--refs", &url], None) {
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
    ) -> Result<String, Error> {
        let url = (self.url)(repo);
        let to = dest.to_string_lossy();
        let mut args = vec!["clone", "--quiet"];
        if !history {
            args.extend(["--depth", "1"]);
        }
        args.extend(["--branch", tag]);
        args.extend([url.as_str(), &to]);
        self.run(&args, None)?;
        Ok(self
            .run(&["rev-parse", "HEAD"], Some(dest))?
            .trim()
            .to_owned())
    }

    /// What changed in `dir` of the clone since commit `old`.
    pub(crate) fn changes(&self, clone: &Path, old: &str, dir: &str) -> String {
        let dir = if dir.is_empty() { "." } else { dir };
        self.run(&["diff", "--stat", old, "HEAD", "--", dir], Some(clone))
            .unwrap_or_else(|_| "The installed commit is not in the repository.\n".into())
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
