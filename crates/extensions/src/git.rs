//! Names, and fetching with the system `git` (`docs/extensions.md`,
//! "Names"). Fiber runs `git` as a person would, so their SSH keys and
//! credential helpers apply.

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Error;

/// The first-party provider extensions' short names.
pub const SHORT_NAMES: [&str; 11] = [
    "anthropic",
    "openai",
    "gemini",
    "codex",
    "openrouter",
    "opencode",
    "databricks",
    "muse",
    "bedrock",
    "vertex",
    "azure",
];

/// What a person typed as a full extension name: a short name becomes
/// `github.com/aakshintala/fiber/providers/<short>`.
pub fn full_name(typed: &str) -> String {
    if SHORT_NAMES.contains(&typed) {
        format!("github.com/aakshintala/fiber/providers/{typed}")
    } else {
        typed.to_owned()
    }
}

/// The name a person types: the short name for a first-party provider,
/// else the name.
///
/// The inverse of [`full_name`]:
/// `github.com/aakshintala/fiber/providers/opencode` shows as `opencode`;
/// any other name shows whole, so a `remove` command naming it works.
pub(crate) fn short_name(name: &str) -> &str {
    const PREFIX: &str = "github.com/aakshintala/fiber/providers/";
    match name.strip_prefix(PREFIX) {
        Some(short) if SHORT_NAMES.contains(&short) => short,
        Some(_) | None => name,
    }
}

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
mod tests {
    use super::{SHORT_NAMES, full_name, is_path, short_name, split};
    use crate::Error;

    #[test]
    fn every_short_name_is_a_first_party_provider() {
        for short in SHORT_NAMES {
            assert_eq!(
                full_name(short),
                format!("github.com/aakshintala/fiber/providers/{short}")
            );
        }
        assert_eq!(SHORT_NAMES.len(), 11);
    }

    #[test]
    fn a_full_name_is_left_alone() {
        assert_eq!(full_name("github.com/acme/x"), "github.com/acme/x");
    }

    #[test]
    fn a_first_party_full_name_shows_short_and_any_other_name_shows_whole() {
        assert_eq!(
            short_name("github.com/aakshintala/fiber/providers/opencode"),
            "opencode"
        );
        assert_eq!(short_name("github.com/acme/lint"), "github.com/acme/lint");
        assert_eq!(
            short_name("github.com/aakshintala/fiber/providers/notashort"),
            "github.com/aakshintala/fiber/providers/notashort"
        );
        assert_eq!(short_name("acme"), "acme");
    }

    #[test]
    fn a_path_is_told_from_a_name() {
        for path in ["./tools/lint", "../lint", "/abs/lint", "~/lint"] {
            assert!(is_path(path), "{path}");
        }
        for name in ["muse", "github.com/acme/lint"] {
            assert!(!is_path(name), "{name}");
        }
    }

    #[test]
    fn a_name_splits_into_repository_and_directory() {
        assert_eq!(split("github.com/a/b").unwrap(), ("github.com/a/b", ""));
        assert_eq!(
            split("github.com/a/b/p/q").unwrap(),
            ("github.com/a/b", "p/q")
        );
        assert_eq!(
            split("gitlab.com/g/s/repo.git/p/q").unwrap(),
            ("gitlab.com/g/s/repo.git", "p/q")
        );
        assert_eq!(
            split("gitlab.com/g/s/repo.git").unwrap(),
            ("gitlab.com/g/s/repo.git", "")
        );
        assert_eq!(
            split("github.com/a/b.git/p").unwrap(),
            ("github.com/a/b.git", "p")
        );
        assert_eq!(split("github.com/a/b/p").unwrap(), ("github.com/a/b", "p"));
        // The first `.git` segment ends the repository; a later one is a directory.
        assert_eq!(
            split("gitlab.com/g/s/repo.git/nested.git/p").unwrap(),
            ("gitlab.com/g/s/repo.git", "nested.git/p")
        );
        assert_eq!(
            split("github.com/a/x.git.git").unwrap(),
            ("github.com/a/x.git.git", "")
        );
        // A bare `.git` segment is not a marker.
        assert_eq!(
            split("gitlab.com/g/s/.git/p").unwrap(),
            ("gitlab.com/g/s", ".git/p")
        );
        // `repo.gitx` is not a marker, so the first three parts stay the repository.
        assert_eq!(
            split("github.com/a/repo.gitx/p").unwrap(),
            ("github.com/a/repo.gitx", "p")
        );
        assert_eq!(split("github.com/a/b/").unwrap(), ("github.com/a/b", ""));
        for bad in [
            "muse",
            "a/b",
            "a//b",
            "/a/b",
            "a/b.git/p",
            "gitlab.com//repo.git/p",
            ".git/p",
            "github.com/a.git/b",
        ] {
            assert!(matches!(split(bad), Err(Error::BadName { .. })), "{bad}");
        }
    }
}
