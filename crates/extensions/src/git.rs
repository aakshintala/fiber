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

/// Whether `typed` names a directory rather than an extension.
pub fn is_path(typed: &str) -> bool {
    typed.starts_with(['.', '/', '~']) || Path::new(typed).join("extension.json").is_file()
}

/// A name as its repository and the directory inside it: the first three
/// segments are the repository (`github.com/owner/repo`).
// ponytail: a host whose repositories sit deeper, such as GitLab subgroups,
// is not told apart; add a rule when someone needs one.
pub(crate) fn split(name: &str) -> Result<(&str, &str), Error> {
    let (repo, dir) = match name.match_indices('/').nth(2) {
        Some((i, _)) => {
            let (repo, rest) = name.split_at(i);
            (repo, rest.strip_prefix('/').unwrap_or(rest))
        }
        None => (name, ""),
    };
    if repo.split('/').count() != 3 || repo.split('/').any(str::is_empty) {
        return Err(Error::BadName { name: name.into() });
    }
    Ok((repo, dir))
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
        let out = self.run(&["ls-remote", "--tags", "--refs", &url], None)?;
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
    use super::{SHORT_NAMES, full_name, is_path, split};

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
        for bad in ["muse", "a/b", "a//b", "/a/b"] {
            assert!(split(bad).is_err(), "{bad}");
        }
    }
}
