//! Builds the user docs site (`docs/ci.md`, "The user docs site").
//!
//! `cargo xtask site --out <dir>` picks the newest stable and newest nightly
//! tag, takes the root tree (the stable, else the nightly), and writes the
//! front page from `research/site-prototype/` at that tag into `<dir>`.
//! Every tag-derived byte comes from git objects at the tag, never from the
//! working tree, so the same tags always build the same tree.

use std::path::Path;
use std::process::Command;

/// The front-page sources the site serves at its root.
const PROTOTYPE_DIR: &str = "research/site-prototype";

/// The one prototype file that never ships: it documents the prototype.
const PROTOTYPE_README: &str = "research/site-prototype/README.md";

/// Runs `site --out <dir>`; usage errors exit 2, a build failure exits 1
/// with one `site: ...` line on stderr.
pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let out = args
        .iter()
        .position(|arg| arg == "--out")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .ok_or("--out <dir> is required".to_owned())?;
    match build(Path::new("."), Path::new(&out)) {
        Ok(()) => Ok(true),
        Err(message) => {
            eprintln!("site: {message}");
            Ok(false)
        }
    }
}

/// Writes the front page at the root tag into `out`.
fn build(root: &Path, out: &Path) -> Result<(), String> {
    prepare_out(out)?;
    let listed = git(root, &["tag", "--list"])?;
    let tags = String::from_utf8(listed)
        .map_err(|error| format!("git tag --list names a tag that is not UTF-8: {error}"))?;
    let (stable, nightly) = select(&tags);
    let root_tag = stable
        .or(nightly)
        .ok_or("no release tag to build from.".to_owned())?;
    let reference = format!("refs/tags/{root_tag}");
    // The full tag ref, so a branch with the tag's name never shadows it.
    let listing = git(
        root,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "-z",
            &reference,
            "--",
            PROTOTYPE_DIR,
        ],
    )?;
    for entry in listing.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(entry)
            .map_err(|error| format!("{PROTOTYPE_DIR} holds a path that is not UTF-8: {error}"))?;
        if path == PROTOTYPE_README {
            continue;
        }
        let relative = path
            .strip_prefix("research/site-prototype/")
            .ok_or(format!("{path} is outside {PROTOTYPE_DIR}."))?;
        let bytes = git(root, &["show", &format!("{reference}:{path}")])?;
        let dest = out.join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&dest, &bytes).map_err(|error| format!("{}: {error}", dest.display()))?;
    }
    Ok(())
}

/// Fails when `out` already holds anything; an empty directory is fresh.
fn prepare_out(out: &Path) -> Result<(), String> {
    if !out.exists() {
        std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
        return Ok(());
    }
    if !out.is_dir() {
        return Err(format!("{} is not a directory.", out.display()));
    }
    let mut entries =
        std::fs::read_dir(out).map_err(|error| format!("{}: {error}", out.display()))?;
    if entries.next().is_some() {
        return Err(format!("{} is not empty.", out.display()));
    }
    Ok(())
}

/// The newest stable tag and the newest nightly tag in `git tag --list`.
fn select(tags: &str) -> (Option<String>, Option<String>) {
    let mut stable: Option<(String, [u64; 3])> = None;
    let mut nightly: Option<(String, ([u64; 3], u64))> = None;
    for line in tags.lines() {
        let tag = line.trim();
        if tag.is_empty() {
            continue;
        }
        if let Some(key) = parse_stable(tag) {
            let incumbent = stable.as_ref().map(|(name, key)| (key, name.as_str()));
            if beats(incumbent, &key, tag) {
                stable = Some((tag.to_owned(), key));
            }
        }
        if let Some(key) = parse_nightly(tag) {
            let incumbent = nightly.as_ref().map(|(name, key)| (key, name.as_str()));
            if beats(incumbent, &key, tag) {
                nightly = Some((tag.to_owned(), key));
            }
        }
    }
    (stable.map(|(name, _)| name), nightly.map(|(name, _)| name))
}

/// Whether a candidate tag beats the incumbent: a higher numeric key wins,
/// and a numeric tie breaks by tag name.
fn beats<K: Ord>(incumbent: Option<(&K, &str)>, key: &K, name: &str) -> bool {
    match incumbent {
        None => true,
        Some((old_key, old_name)) => key > old_key || (*key == *old_key && name > old_name),
    }
}

/// One `v`-less version component: `0` or digits without a leading zero
/// that fit a `u64`, so overflow and `01` never parse.
fn component(text: &str) -> Option<u64> {
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok()
}

/// `v<major>.<minor>.<patch>` compared as three integers.
fn parse_stable(tag: &str) -> Option<[u64; 3]> {
    let dotted = tag.strip_prefix('v')?;
    let mut parts = dotted.split('.');
    let key = [
        component(parts.next()?)?,
        component(parts.next()?)?,
        component(parts.next()?)?,
    ];
    if parts.next().is_some() {
        return None;
    }
    Some(key)
}

/// `v<major>.<minor>.<patch>-nightly.<YYYYMMDD>` compared as four integers.
fn parse_nightly(tag: &str) -> Option<([u64; 3], u64)> {
    let (base, date) = tag.split_once("-nightly.")?;
    if date.len() != 8 || !date.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((parse_stable(base)?, date.parse::<u64>().ok()?))
}

/// Runs `git args` in `root` and returns its standard output.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), detail.trim()));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_parses_three_plain_components() {
        assert_eq!(parse_stable("v0.1.10"), Some([0, 1, 10]));
        assert_eq!(parse_stable("v0.1.0"), Some([0, 1, 0]));
    }

    #[test]
    fn stable_rejects_suffixes_names_and_bad_numbers() {
        assert_eq!(parse_stable("v0.1.0-rc1"), None);
        assert_eq!(parse_stable("v0.1.0-nightly.20261023"), None);
        assert_eq!(parse_stable("docs-x"), None);
        assert_eq!(parse_stable("v01.2.3"), None);
        assert_eq!(parse_stable("v99999999999999999999.0.0"), None);
        assert_eq!(parse_stable("v0.1"), None);
        assert_eq!(parse_stable("v0.1.0.0"), None);
    }

    #[test]
    fn nightly_parses_an_eight_digit_date() {
        assert_eq!(
            parse_nightly("v0.4.1-nightly.20261101"),
            Some(([0, 4, 1], 20261101))
        );
        assert_eq!(parse_nightly("v0.4.1-nightly.2026110"), None);
        assert_eq!(parse_nightly("v0.4.1-nightly.202611011"), None);
        assert_eq!(parse_nightly("v01.4.1-nightly.20261101"), None);
        assert_eq!(parse_nightly("v0.4.1"), None);
    }

    #[test]
    fn a_higher_key_beats_a_lower_one_either_way() {
        assert!(beats::<[u64; 3]>(
            Some((&[0, 1, 9], "v0.1.9")),
            &[0, 1, 10],
            "v0.1.10"
        ));
        assert!(!beats::<[u64; 3]>(
            Some((&[0, 1, 10], "v0.1.10")),
            &[0, 1, 9],
            "v0.1.9"
        ));
    }

    #[test]
    fn a_numeric_tie_breaks_by_tag_name() {
        assert!(beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "b"));
        assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "b")), &[0, 1, 0], "a"));
        assert!(!beats::<[u64; 3]>(Some((&[0, 1, 0], "a")), &[0, 1, 0], "a"));
    }

    #[test]
    fn the_first_tag_has_nothing_to_beat() {
        assert!(beats::<[u64; 3]>(None, &[0, 1, 0], "v0.1.0"));
    }

    #[test]
    fn select_picks_each_channel_maximum() {
        let (stable, nightly) =
            select("v0.1.9\nv0.1.10\nv0.2.0-nightly.20261031\nv0.1.9-nightly.20261231\ndocs-x\n");
        assert_eq!(stable.as_deref(), Some("v0.1.10"));
        assert_eq!(nightly.as_deref(), Some("v0.2.0-nightly.20261031"));
    }

    #[test]
    fn select_is_empty_without_release_tags() {
        assert_eq!(select("docs-x\nv0.1.0-rc1\n"), (None, None));
    }
}
